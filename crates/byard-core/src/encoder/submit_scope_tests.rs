use super::*;

fn try_device() -> Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>, byard_test_gpu::Turn)> {
    byard_test_gpu::device(crate::engine::device_limits)
}

#[test]
fn submitting_a_command_buffer_enters_encode_submit() {
    let Some((device, queue, _turn)) = try_device() else {
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
