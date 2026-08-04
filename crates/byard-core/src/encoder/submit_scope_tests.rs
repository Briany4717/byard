
use super::*;

fn try_device() -> Option<(std::sync::Arc<wgpu::Device>, std::sync::Arc<wgpu::Queue>)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ByardCore - encode.submit Test Device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::Performance,
        ..Default::default()
    }))
    .ok()?;
    Some((std::sync::Arc::new(device), std::sync::Arc::new(queue)))
}

#[test]
fn submitting_a_command_buffer_enters_encode_submit() {
    let Some((device, queue)) = try_device() else {
        eprintln!("no GPU adapter available, skipping");
        return;
    };
    let mut enc = pollster::block_on(EncoderSubsystem::init(
        std::sync::Arc::clone(&device),
        std::sync::Arc::clone(&queue),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        1.0,
        32,
        32,
    ))
    .expect("encoder init");

    let empty = device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None })
        .finish();
    let _ = crate::telemetry::drain_samples();
    enc.submit(empty);
    device.poll(wgpu::PollType::wait_indefinitely()).ok();

    let block = crate::telemetry::drain_samples();
    assert!(
        block
            .samples
            .iter()
            .any(|s| crate::telemetry::scope_name(s.scope) == Some("encode.submit")),
        "encode.submit was never entered, the queue submission has stopped \
             being measured, so upload cost flushed at submit time is invisible"
    );
}
