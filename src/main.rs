use sctk::reexports::client::protocol::wl_shm::{Format, WlShm};
use sctk::reexports::client::{protocol::wl_output::WlOutput, Display, GlobalManager};
use sctk::reexports::protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use smithay_client_toolkit as sctk;
use std::fs::File;
use std::os::unix::io::AsRawFd;

fn main() {
    let display = Display::connect_to_env().unwrap();
    let mut event_queue = display.create_event_queue();
    let attached_display = display.attach(event_queue.token());
    let registry = attached_display.get_registry();
    let globals = GlobalManager::new(&attached_display);

    event_queue.sync_roundtrip(&mut (), |_, _, _| {}).unwrap();

    let screencopy_manager = globals
        .instantiate_exact::<ZwlrScreencopyManagerV1>(1)
        .expect("Unable to init screencopy_manager");

    let output = registry.bind::<WlOutput>(1, 1);
    let shm = registry.bind::<WlShm>(1, 1);

    let fd: File = tempfile::tempfile().unwrap();
    let shm_pool = shm.create_pool(fd.as_raw_fd(), 2147483647);

    let frame = screencopy_manager.capture_output(0, &output);
    let buffer = shm_pool.create_buffer(0, 1920, 1080, 5760, Format::Argb8888);
    frame.copy(&buffer);

    frame.destroy();
    buffer.destroy();
    shm_pool.destroy();
    screencopy_manager.destroy();
}
