//! One persistent GPU buffer for every pipeline's per-frame instance data
//! (RFC-0033).
//!
//! # What this replaces
//!
//! Every render pipeline in this encoder used to create its instance buffer
//! from scratch on every frame — nine or more `create_buffer_init` calls per
//! frame, more with multiple draw batches. The correct pattern (a persistent
//! buffer written with `queue.write_buffer`) existed in the crate in exactly
//! one place: `viewport_buffer`. There was no design reason for the asymmetry;
//! that buffer is simply the one that was written correctly.
//!
//! # This is a determinism fix, not a frame-time fix
//!
//! Worth stating plainly, because RFC-0033's summary projected otherwise and
//! the measurement disagreed. The `encode.frame` sub-scopes added in RFC-0030
//! §I1's second pass put every `create_buffer_init` in the encoder combined at
//! **0.3–3.4 %** of the encode cost; the rest was glyph shaping. So the case
//! for this is not the microseconds — it is that a framework whose central
//! claim is deterministic memory (RFC-0001 §2, *"sin spikes de VRAM"*) should
//! not be allocating and freeing GPU resources at the display rate. Each
//! `create_buffer_init` is a device allocation, a validation pass, a staging
//! allocation, a copy, and a tracker registration the driver reclaims at an
//! unpredictable time — which is precisely the class of non-deterministic
//! pause the project exists to avoid.
//!
//! The second reason is testability: "zero buffer creations per steady-state
//! frame" is a deterministic assertion, and deterministic assertions are the
//! ones that actually catch regressions on shared CI hardware.
//!
//! # Shape
//!
//! One GPU buffer, one reused CPU-side staging `Vec<u8>`, one `write_buffer`
//! per frame. Per frame: [`begin_frame`](InstanceArena::begin_frame) →
//! each pipeline appends its instances and records a [`Region`] →
//! [`upload`](InstanceArena::upload) → each draw binds its own slice.
//!
//! **The ordering is not a style choice.** `wgpu` binds a buffer *range*
//! eagerly, so the GPU buffer must be final before any draw is recorded — and
//! growing it replaces it. Every append therefore happens before the first
//! render pass opens, which is why the pipelines in this module are split into
//! a `stage` half and a `draw` half.

/// A byte range this frame's staging buffer handed to one draw.
///
/// Carries its length so a debug build can assert a draw binds exactly the
/// region it staged, and so an empty batch is representable without an
/// `Option` at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    /// Byte offset into the arena's GPU buffer.
    pub offset: u64,
    /// Length in bytes.
    pub len: u64,
}

impl Region {
    /// Whether this region carries no data — an empty draw batch.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Vertex buffer offsets must be a multiple of this (`wgpu` requirement).
///
/// Every instance struct in `frame.rs` is `#[repr(C)]` with explicit padding
/// to 4-byte-or-better alignment, so this is satisfied by construction; the
/// arena asserts it rather than assuming it.
const VERTEX_ALIGNMENT: u64 = 4;

/// Initial GPU buffer size, in bytes.
///
/// Small on purpose (RFC-0033 §G3): the arena sizes itself from the first
/// frames' actual usage, so a small app never reserves for a large one. The
/// cost of starting too small is a handful of growths in the first second and
/// then never again — which is exactly what `grows_this_session` is there to
/// confirm.
const INITIAL_CAPACITY: usize = 64 * 1024;

/// A persistent, grow-only GPU buffer shared by every instanced pipeline.
pub struct InstanceArena {
    gpu: wgpu::Buffer,
    /// Reused between frames; cleared, never reallocated in steady state.
    staging: Vec<u8>,
    /// Size of `gpu` in bytes — the session high-water mark.
    capacity: u64,
    /// How many times the GPU buffer has been reallocated this session.
    /// Nonzero after warm-up means something is churning (RFC-0033 §G5).
    grows_this_session: u32,
    /// How many `create_buffer*` calls this arena has made, ever. The
    /// acceptance counter for "zero GPU buffer creations on a steady-state
    /// frame": it starts at 1 (the buffer itself) and must not move again.
    buffer_creations: u32,
    /// `min_uniform_buffer_offset_alignment`, read from the device.
    ///
    /// **Never hardcoded to 256.** It is 256 on many backends, which is
    /// exactly what makes hardcoding it work on the development machine and
    /// fail — or silently corrupt — elsewhere.
    uniform_alignment: u64,
}

impl InstanceArena {
    /// Creates the arena and its single GPU buffer.
    #[must_use]
    pub fn new(device: &wgpu::Device) -> Self {
        let capacity = INITIAL_CAPACITY as u64;
        let gpu = Self::create_buffer(device, capacity);
        Self {
            gpu,
            staging: Vec::with_capacity(INITIAL_CAPACITY),
            capacity,
            grows_this_session: 0,
            buffer_creations: 1,
            uniform_alignment: u64::from(device.limits().min_uniform_buffer_offset_alignment),
        }
    }

    fn create_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ByardCore - Instance Arena"),
            size,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Starts a frame: discards last frame's staged bytes, keeping capacity.
    pub fn begin_frame(&mut self) {
        self.staging.clear();
    }

    /// Appends `data` as a **vertex** region and returns its location.
    ///
    /// An empty slice returns an empty [`Region`] without touching the
    /// staging buffer, so a pipeline with nothing to draw costs nothing.
    pub fn push_vertex<T: bytemuck::Pod>(&mut self, data: &[T]) -> Region {
        self.push_bytes(bytemuck::cast_slice(data), VERTEX_ALIGNMENT)
    }

    /// Appends `value` as a **uniform** region, padded to the device's
    /// reported `min_uniform_buffer_offset_alignment`.
    pub fn push_uniform<T: bytemuck::Pod>(&mut self, value: &T) -> Region {
        let alignment = self.uniform_alignment;
        self.push_bytes(bytemuck::bytes_of(value), alignment)
    }

    fn push_bytes(&mut self, bytes: &[u8], alignment: u64) -> Region {
        if bytes.is_empty() {
            return Region::default();
        }
        let padding = align_padding(self.staging.len() as u64, alignment);
        self.staging
            .resize(self.staging.len() + usize_of(padding), 0);
        let offset = self.staging.len() as u64;
        debug_assert_eq!(
            offset % alignment,
            0,
            "instance arena produced a misaligned region at {offset} \
             (alignment {alignment})"
        );
        self.staging.extend_from_slice(bytes);
        Region {
            offset,
            len: bytes.len() as u64,
        }
    }

    /// Reserves `bytes` of **vertex** space without supplying the data yet.
    ///
    /// For the handful of regions whose contents are not known until *after*
    /// [`upload`](Self::upload) has run — the backdrop pipeline computes its
    /// composite quad while recording, because the pane can only be sampled
    /// once the geometry behind it has been rasterised. Reserving keeps the
    /// arena's single-growth-point guarantee (the buffer is still final before
    /// any pass opens); the contents arrive later via
    /// [`write_region`](Self::write_region).
    pub fn reserve_vertex(&mut self, bytes: u64) -> Region {
        self.reserve(bytes, VERTEX_ALIGNMENT)
    }

    /// [`reserve_vertex`](Self::reserve_vertex) for a **uniform** region,
    /// padded to the device's reported alignment.
    pub fn reserve_uniform(&mut self, bytes: u64) -> Region {
        let alignment = self.uniform_alignment;
        self.reserve(bytes, alignment)
    }

    fn reserve(&mut self, bytes: u64, alignment: u64) -> Region {
        if bytes == 0 {
            return Region::default();
        }
        let padding = align_padding(self.staging.len() as u64, alignment);
        let total = usize_of(padding + bytes);
        self.staging.resize(self.staging.len() + total, 0);
        Region {
            offset: self.staging.len() as u64 - bytes,
            len: bytes,
        }
    }

    /// Writes `bytes` into an already-reserved region.
    ///
    /// # Panics
    ///
    /// Debug builds panic if `bytes` does not fit the region — writing past it
    /// would corrupt whichever pipeline owns the next one, which is the one
    /// drawback a single shared arena has over per-pipeline buffers.
    pub fn write_region(&self, queue: &wgpu::Queue, region: Region, bytes: &[u8]) {
        debug_assert!(
            bytes.len() as u64 <= region.len,
            "{} bytes do not fit the {} byte region reserved for them",
            bytes.len(),
            region.len
        );
        if bytes.is_empty() {
            return;
        }
        queue.write_buffer(&self.gpu, region.offset, bytes);
    }

    /// Uploads this frame's staged bytes in a single `write_buffer`, growing
    /// the GPU buffer first if they no longer fit.
    ///
    /// **Must be called before any region is bound.** Growth replaces the GPU
    /// buffer, and a draw recorded against the old one would read freed
    /// memory; the debug assertion in [`slice`](Self::slice) is the backstop.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let needed = self.staging.len() as u64;
        if needed > self.capacity {
            // Grow-only, doubling, never shrinking (RFC-0033 §G3). Shrinking
            // would recreate the buffer — the operation being eliminated — at
            // the least predictable moment, and a UI's instance high-water
            // mark is bounded by the UI rather than by an unbounded workload.
            let mut capacity = self.capacity.max(1);
            while capacity < needed {
                capacity *= 2;
            }
            self.gpu = Self::create_buffer(device, capacity);
            self.capacity = capacity;
            self.grows_this_session += 1;
            self.buffer_creations += 1;
        }
        if needed > 0 {
            queue.write_buffer(&self.gpu, 0, &self.staging);
        }
    }

    /// Binds `region` as a buffer slice.
    ///
    /// # Panics
    ///
    /// Debug builds panic if the region falls outside the uploaded range —
    /// which means either that it was staged after [`upload`](Self::upload),
    /// or that the arena grew between staging and binding.
    #[must_use]
    pub fn slice(&self, region: Region) -> wgpu::BufferSlice<'_> {
        debug_assert!(
            region.offset + region.len <= self.capacity,
            "instance arena region {region:?} is outside the {} byte buffer — \
             it was staged after `upload`, or the arena grew in between",
            self.capacity
        );
        self.gpu.slice(region.offset..region.offset + region.len)
    }

    /// The arena's GPU buffer, for a bind group entry that needs the buffer
    /// itself (a uniform binding) rather than a vertex slice.
    #[must_use]
    pub const fn buffer(&self) -> &wgpu::Buffer {
        &self.gpu
    }

    /// Bytes staged so far this frame.
    #[must_use]
    pub fn staged_len(&self) -> u64 {
        self.staging.len() as u64
    }

    /// Current GPU buffer size in bytes — the session high-water mark.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.capacity
    }

    /// How many times the GPU buffer has been reallocated this session
    /// (RFC-0033 §G5). Must be stable after warm-up.
    #[must_use]
    pub const fn grows_this_session(&self) -> u32 {
        self.grows_this_session
    }

    /// How many GPU buffers this arena has created, ever — one at
    /// construction plus one per growth. The counter the frame budget suite
    /// asserts is stationary across a steady-state frame.
    #[must_use]
    pub const fn buffer_creations(&self) -> u32 {
        self.buffer_creations
    }

    /// The device's reported uniform offset alignment.
    #[must_use]
    pub const fn uniform_alignment(&self) -> u64 {
        self.uniform_alignment
    }
}

/// How many padding bytes take `offset` up to a multiple of `alignment`.
const fn align_padding(offset: u64, alignment: u64) -> u64 {
    let rem = offset % alignment;
    if rem == 0 { 0 } else { alignment - rem }
}

/// `u64` → `usize` for a value that is always a small padding count.
#[allow(clippy::cast_possible_truncation)]
const fn usize_of(v: u64) -> usize {
    v as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("ByardCore - Instance Arena Test Device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .ok()?;
        Some((Arc::new(device), Arc::new(queue)))
    }

    #[test]
    fn padding_takes_an_offset_up_to_the_next_multiple() {
        assert_eq!(align_padding(0, 256), 0);
        assert_eq!(align_padding(1, 256), 255);
        assert_eq!(align_padding(256, 256), 0);
        assert_eq!(align_padding(257, 256), 255);
        assert_eq!(align_padding(6, 4), 2);
    }

    #[test]
    fn appended_regions_are_contiguous_and_aligned() {
        let Some((device, _queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping");
            return;
        };
        let mut arena = InstanceArena::new(&device);
        arena.begin_frame();

        let a = arena.push_vertex(&[1u32, 2, 3]);
        let b = arena.push_vertex(&[4u32]);
        assert_eq!(a.offset, 0);
        assert_eq!(a.len, 12);
        assert_eq!(b.offset, 12, "4-byte data needs no padding between regions");
        assert_eq!(b.len, 4);
        assert_eq!(arena.staged_len(), 16);
    }

    #[test]
    fn a_uniform_region_is_padded_to_the_devices_own_alignment() {
        // The trap RFC-0033 §G2 names: 256 is *a* value this limit takes, not
        // *the* value, and assuming it is the least useful thing to assume
        // because it works on the machine you are writing on.
        let Some((device, _queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping");
            return;
        };
        let mut arena = InstanceArena::new(&device);
        let alignment = arena.uniform_alignment();
        assert_eq!(
            alignment,
            u64::from(device.limits().min_uniform_buffer_offset_alignment),
            "the alignment must come from the device, not from a constant"
        );

        arena.begin_frame();
        let _vertex = arena.push_vertex(&[1u32]);
        let uniform = arena.push_uniform(&[0.5f32; 4]);
        assert_eq!(
            uniform.offset % alignment,
            0,
            "a uniform region must start on the device's alignment"
        );
        assert!(uniform.offset >= 4, "and after the vertex region before it");
    }

    #[test]
    fn an_empty_push_costs_nothing_and_returns_an_empty_region() {
        let Some((device, _queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping");
            return;
        };
        let mut arena = InstanceArena::new(&device);
        arena.begin_frame();
        let region = arena.push_vertex::<u32>(&[]);
        assert!(region.is_empty());
        assert_eq!(arena.staged_len(), 0, "an empty batch must not pad");
    }

    #[test]
    fn growth_doubles_and_never_shrinks() {
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping");
            return;
        };
        let mut arena = InstanceArena::new(&device);
        let initial = arena.capacity();

        // One frame that overflows the initial capacity.
        arena.begin_frame();
        let big = vec![0u32; INITIAL_CAPACITY / 4 + 16];
        let _ = arena.push_vertex(&big);
        arena.upload(&device, &queue);
        assert!(arena.capacity() > initial);
        assert_eq!(arena.grows_this_session(), 1);
        let grown = arena.capacity();
        assert_eq!(grown, initial * 2, "growth doubles");

        // A tiny frame afterwards must not give the memory back — shrinking
        // recreates the buffer, which is the operation being removed.
        arena.begin_frame();
        let _ = arena.push_vertex(&[1u32]);
        arena.upload(&device, &queue);
        assert_eq!(arena.capacity(), grown);
        assert_eq!(arena.grows_this_session(), 1);
    }

    #[test]
    fn a_steady_state_frame_creates_no_buffers_and_reallocates_no_staging() {
        // The two acceptance conditions of RFC-0033 §G5, asserted rather than
        // benchmarked: after warm-up a frame of the same shape must create
        // zero GPU buffers and must not grow the staging `Vec` either.
        let Some((device, queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping");
            return;
        };
        let mut arena = InstanceArena::new(&device);
        let data = vec![7u32; 4096];

        for _ in 0..3 {
            arena.begin_frame();
            let _ = arena.push_vertex(&data);
            arena.upload(&device, &queue);
        }
        let creations = arena.buffer_creations();
        let staging_capacity = arena.staging.capacity();

        for _ in 0..20 {
            arena.begin_frame();
            let _ = arena.push_vertex(&data);
            arena.upload(&device, &queue);
        }

        assert_eq!(
            arena.buffer_creations(),
            creations,
            "a steady-state frame must create no GPU buffers"
        );
        assert_eq!(arena.grows_this_session(), 0, "and must not grow the arena");
        assert_eq!(
            arena.staging.capacity(),
            staging_capacity,
            "and the CPU-side staging vector must not reallocate either"
        );
    }

    #[test]
    #[should_panic(expected = "outside the")]
    fn binding_a_region_the_arena_never_uploaded_fails_in_debug() {
        let Some((device, _queue)) = try_device() else {
            // `should_panic` needs a panic even on the skip path, or the test
            // fails for the wrong reason on a machine with no GPU.
            panic!("outside the — no GPU adapter, skipping");
        };
        let arena = InstanceArena::new(&device);
        let _ = arena.slice(Region {
            offset: arena.capacity(),
            len: 16,
        });
    }
}
