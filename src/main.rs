use std::{
    cell::RefCell,
    ffi::CStr,
    fs::File,
    io::Write,
    os::unix::prelude::{FromRawFd, RawFd},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Result};
use image::{codecs::jpeg::JpegEncoder, ImageEncoder};
use memmap2::MmapMut;
use nix::{
    errno::Errno,
    fcntl,
    sys::{memfd, mman, stat},
    unistd,
};
use tracing::debug;
use wayland_client::{
    protocol::{wl_output, wl_shm},
    Display, GlobalManager, Main,
};
use wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_manager_v1;

#[derive(Debug, Copy, Clone)]
struct FrameFormat {
    format: wayland_client::protocol::wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}

#[derive(Debug, Copy, Clone)]
enum FrameState {
    Failed,
    Finished,
}

fn main() -> Result<()> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .init();
    }

    let display = Display::connect_to_env()?;
    let mut event_queue = display.create_event_queue();
    let attached_display = (*display).clone().attach(event_queue.token());

    let globals = GlobalManager::new(&attached_display);
    event_queue.sync_roundtrip(&mut (), |_, _, _| unreachable!())?;

    let outputs: Rc<RefCell<Vec<Main<wl_output::WlOutput>>>> = Rc::new(RefCell::new(Vec::new()));
    let outputs_done = Rc::new(AtomicBool::new(false));
    let output_global = globals.instantiate_exact::<wl_output::WlOutput>(2)?;
    output_global.quick_assign({
        let outputs = outputs.clone();
        let outputs_done = outputs_done.clone();
        move |wl_output, event, _| {
            outputs.borrow_mut().push(wl_output);
            match event {
                wayland_client::protocol::wl_output::Event::Geometry { .. } => {}
                wayland_client::protocol::wl_output::Event::Mode { .. } => {}
                wayland_client::protocol::wl_output::Event::Done => {
                    outputs_done.store(true, Ordering::SeqCst);
                }
                wayland_client::protocol::wl_output::Event::Scale { .. } => {}
                _ => unreachable!(),
            }
        }
    });

    while !outputs_done.load(Ordering::SeqCst) {
        event_queue.sync_roundtrip(&mut (), |_, _, _| unreachable!())?;
    }

    let output = match outputs.borrow().first().cloned() {
        Some(output) => output,
        None => bail!("compositor did not advertise a output"),
    };

    let frame_formats: Rc<RefCell<Vec<FrameFormat>>> = Rc::new(RefCell::new(Vec::new()));
    let frame_state: Rc<RefCell<Option<FrameState>>> = Rc::new(RefCell::new(None));
    let frame_buffer_done = Rc::new(AtomicBool::new(false));

    let screencopy_manager =
        globals.instantiate_exact::<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>(3)?;
    let frame = screencopy_manager.capture_output(0, &output.detach());
    frame.quick_assign({
        let frame_formats = frame_formats.clone();
        let frame_state = frame_state.clone();
        let frame_buffer_done = frame_buffer_done.clone();
        move |_frame, event, _| {
        match event {
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                frame_formats.borrow_mut().push(FrameFormat {
                    format,
                    width,
                    height,
                    stride,
                });
            },
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::Flags { .. } => {},
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                frame_state.borrow_mut().replace(FrameState::Finished);
            },
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::Failed => {
                frame_state.borrow_mut().replace(FrameState::Failed);
            },
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::Damage { .. } => {},
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::LinuxDmabuf { .. } => {},
            wayland_protocols::wlr::unstable::screencopy::v1::client::zwlr_screencopy_frame_v1::Event::BufferDone => {
                frame_buffer_done.store(true, Ordering::SeqCst);
            },
            _ => unreachable!()
        };
    }});

    while !frame_buffer_done.load(Ordering::SeqCst) {
        event_queue.sync_roundtrip(&mut (), |_, _, _| unreachable!())?;
    }

    debug!(formats = ?frame_formats, "received compositor frame buffer formats");

    let frame_format = frame_formats
        .borrow()
        .iter()
        .filter(|f| {
            matches!(
                f.format,
                wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888
            )
        })
        .nth(0)
        .copied();

    debug!(format = ?frame_format, "selected frame buffer format");

    let frame_format = match frame_format {
        Some(format) => format,
        None => bail!("no suitable frame format found"),
    };

    let frame_bytes = frame_format.stride * frame_format.height;

    let mem_fd = create_shm_fd()?;
    let mem_file = unsafe { File::from_raw_fd(mem_fd) };
    mem_file.set_len(frame_bytes as u64)?;

    let shm = globals.instantiate_exact::<wl_shm::WlShm>(1)?;
    let pool = shm.create_pool(mem_fd, frame_bytes as i32);
    let buffer = pool.create_buffer(
        0,
        frame_format.width as i32,
        frame_format.height as i32,
        frame_format.stride as i32,
        frame_format.format,
    );

    frame.copy(&buffer);

    let result = loop {
        event_queue.sync_roundtrip(&mut (), |_, _, _| {})?;

        if let Some(state) = frame_state.borrow_mut().take() {
            match state {
                FrameState::Failed => {
                    break Err(anyhow::anyhow!("frame copy failed"));
                }
                FrameState::Finished => {
                    let mut mmap = unsafe { MmapMut::map_mut(&mem_file)? };
                    let stdout = std::io::stdout();
                    let guard = stdout.lock();
                    let mut writer = std::io::BufWriter::new(guard);
                    let data = &mut *mmap;
                    let color_type = match frame_format.format {
                        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => {
                            for chunk in data.chunks_exact_mut(4) {
                                let tmp = chunk[0];
                                chunk[0] = chunk[2];
                                chunk[2] = tmp;
                            }
                            image::ColorType::Rgba8
                        }
                        wl_shm::Format::Xbgr8888 => image::ColorType::Rgba8,
                        other => {
                            break Err(anyhow::anyhow!("Unsupported buffer format: {:?}", other))
                        }
                    };
                    JpegEncoder::new(&mut writer).write_image(
                        &mmap,
                        frame_format.width,
                        frame_format.height,
                        color_type,
                    )?;
                    writer.flush()?;
                    break Ok(());
                }
            }
        }
    };

    result
}

fn create_shm_fd() -> std::io::Result<RawFd> {
    // Only try memfd on linux
    #[cfg(target_os = "linux")]
    loop {
        match memfd::memfd_create(
            CStr::from_bytes_with_nul(b"wayshot\0").unwrap(),
            memfd::MemFdCreateFlag::MFD_CLOEXEC | memfd::MemFdCreateFlag::MFD_ALLOW_SEALING,
        ) {
            Ok(fd) => {
                // this is only an optimization, so ignore errors
                let _ = fcntl::fcntl(
                    fd,
                    fcntl::F_ADD_SEALS(
                        fcntl::SealFlag::F_SEAL_SHRINK | fcntl::SealFlag::F_SEAL_SEAL,
                    ),
                );
                return Ok(fd);
            }
            Err(nix::Error::Sys(Errno::EINTR)) => continue,
            Err(nix::Error::Sys(Errno::ENOSYS)) => break,
            Err(nix::Error::Sys(errno)) => return Err(std::io::Error::from(errno)),
            Err(err) => unreachable!(err),
        }
    }

    // Fallback to using shm_open
    let sys_time = SystemTime::now();
    let mut mem_file_handle = format!(
        "/wayshot-{}",
        sys_time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
    );
    loop {
        match mman::shm_open(
            mem_file_handle.as_str(),
            fcntl::OFlag::O_CREAT
                | fcntl::OFlag::O_EXCL
                | fcntl::OFlag::O_RDWR
                | fcntl::OFlag::O_CLOEXEC,
            stat::Mode::S_IRUSR | stat::Mode::S_IWUSR,
        ) {
            Ok(fd) => match mman::shm_unlink(mem_file_handle.as_str()) {
                Ok(_) => return Ok(fd),
                Err(nix::Error::Sys(errno)) => match unistd::close(fd) {
                    Ok(_) => return Err(std::io::Error::from(errno)),
                    Err(nix::Error::Sys(errno)) => return Err(std::io::Error::from(errno)),
                    Err(err) => panic!("{}", err),
                },
                Err(err) => panic!("{}", err),
            },
            Err(nix::Error::Sys(Errno::EEXIST)) => {
                // If a file with that handle exists then change the handle
                mem_file_handle = format!(
                    "/wayshot-{}",
                    sys_time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
                );
                continue;
            }
            Err(nix::Error::Sys(Errno::EINTR)) => continue,
            Err(nix::Error::Sys(errno)) => return Err(std::io::Error::from(errno)),
            Err(err) => unreachable!(err),
        }
    }
}
