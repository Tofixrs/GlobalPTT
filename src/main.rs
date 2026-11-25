// PipeWire's loopback module builds the virtual adapter node. This program
// controls its mute state directly through PipeWire when the portal shortcut
// is pressed and released.
use std::{
    cell::{Cell, RefCell},
    env,
    ffi::CString,
    rc::Rc,
    thread,
    time::Duration,
};

use anyhow::Context;
use ashpd::{
    AppID,
    desktop::{
        CreateSessionOptions,
        global_shortcuts::{GlobalShortcuts, NewShortcut},
    },
    register_host_app_with_connection,
};
use futures_util::StreamExt;
use pipewire as pw;
use pw::{
    properties::properties,
    spa::{
        param::ParamType,
        pod::{Object, Pod, Property, PropertyFlags, Value, serialize::PodSerializer},
    },
    types::ObjectType,
};
use zbus::Connection;

const APP_ID: &str = "io.github.tofixrs.GlobalPTT";

const FILTER_NAME: &str = "GlobalPTT Virtual Microphone";

const SOURCE_NAME: &str = "globalptt_virtual_mic";

const TONE_RATE: u32 = 48_000;
const TONE_CHANNELS: u32 = 2;
const TONE_FRAMES: u32 = 3_360;

fn load_loopback(context: &pw::context::ContextRc) -> anyhow::Result<()> {
    let target = env::var("GLOBALPTT_MIC")
        .map(|t| format!("target.object = \"{t}\""))
        .unwrap_or_default();
    let args = CString::new(format!(
        r#"{{
            audio.rate = 48000
            audio.position = [ FL FR ]
            capture.props = {{
                node.name = "globalptt_capture"
                node.passive = true
                {target}
            }}
            playback.props = {{
                node.name = "{SOURCE_NAME}"
                node.description = "{FILTER_NAME}"
                media.class = "Audio/Source"
                node.virtual = true
            }}
        }}"#
    ))?;
    let module = CString::new("libpipewire-module-loopback")?;
    let handle = unsafe {
        pw::sys::pw_context_load_module(
            context.as_raw_ptr(),
            module.as_ptr(),
            args.as_ptr(),
            std::ptr::null_mut(),
        )
    };
    anyhow::ensure!(!handle.is_null(), "could not load the loopback module");
    Ok(())
}

fn apply_mute(node: &pw::node::Node, muted: bool) {
    let object = Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pw::spa::sys::SPA_PARAM_Props,
        properties: vec![Property {
            key: pw::spa::sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(muted),
        }],
    };
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .unwrap()
        .0
        .into_inner();
    if let Some(pod) = Pod::from_bytes(&bytes) {
        node.set_param(ParamType::Props, 0, pod);
    }
}

fn run_global_shortcuts(mute_tx: pw::channel::Sender<bool>) -> anyhow::Result<()> {
    for attempt in 1..=3 {
        match run_global_shortcuts_once(mute_tx.clone()) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let is_disconnect = format!("{error:#}").contains("NoReply");
                if !is_disconnect || attempt == 3 {
                    return Err(error);
                }
                let _ = mute_tx.send(true);
                eprintln!("Portal D-Bus peer disconnected; reconnecting (attempt {attempt}/3).");
                thread::sleep(Duration::from_millis(500));
            }
        }
    }
    unreachable!()
}

fn run_global_shortcuts_once(mute_tx: pw::channel::Sender<bool>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start portal async runtime")?;
    runtime.block_on(run_global_shortcuts_async(mute_tx))
}

async fn run_global_shortcuts_async(mute_tx: pw::channel::Sender<bool>) -> anyhow::Result<()> {
    let connection = Connection::session()
        .await
        .context("connect to the session D-Bus")?;
    let app_id = AppID::try_from(APP_ID)?;
    register_host_app_with_connection(connection.clone(), app_id)
        .await
        .context("register the app ID with the host portal")?;

    let portal = GlobalShortcuts::with_connection(connection)
        .await
        .context("connect to the Global Shortcuts portal")?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .context("create a Global Shortcuts session")?;

    // Subscribe before BindShortcuts completes so an immediate activation
    // cannot be missed after the user confirms the portal dialog.
    let mut activated = portal.receive_activated().await?;
    let mut deactivated = portal.receive_deactivated().await?;
    let shortcut = NewShortcut::new("push-to-talk", "Hold to talk");
    portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await?
        .response()?;

    println!(
        "Push-to-talk shortcut registered. Hold the configured shortcut to send microphone audio."
    );
    loop {
        tokio::select! {
            event = activated.next() => {
                let event = event.ok_or_else(|| anyhow::anyhow!("portal activation stream closed"))?;
                if event.shortcut_id() == "push-to-talk" {
                    let _ = mute_tx.send(false);
                }
            }
            event = deactivated.next() => {
                let event = event.ok_or_else(|| anyhow::anyhow!("portal deactivation stream closed"))?;
                if event.shortcut_id() == "push-to-talk" {
                    let _ = mute_tx.send(true);
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    load_loopback(&context)?;

    // A separate notification stream keeps the cue audible while the virtual
    // microphone is muted. The process callback only generates a short tone
    // after a portal activation or deactivation event.
    let tone_request = Rc::new(Cell::new(None));
    let tone_stream = pw::stream::StreamBox::new(
        &core,
        "GlobalPTT notification",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Notification",
            *pw::keys::AUDIO_CHANNELS => "2",
        },
    )?;
    let _tone_listener = tone_stream
        .add_local_listener_with_user_data((Rc::clone(&tone_request), 0.0_f64, 0_u32, 880.0_f64))
        .process(|stream, (request, phase, remaining, frequency)| {
            if let Some(enabled) = request.take() {
                *phase = 0.0;
                *remaining = TONE_FRAMES;
                // Two distinct pitches make press and release easy to tell apart.
                *frequency = if enabled { 880.0 } else { 440.0 };
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(samples) = data.data() else {
                return;
            };
            let stride = std::mem::size_of::<i16>() * TONE_CHANNELS as usize;
            let frames = samples.len() / stride;
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (frames * stride) as _;
            let samples = data.data().unwrap();
            for frame in 0..frames {
                let sample = if *remaining > 0 {
                    let value = (phase.sin() * 0.12 * i16::MAX as f64) as i16;
                    *phase += std::f64::consts::TAU * *frequency / TONE_RATE as f64;
                    if *phase >= std::f64::consts::TAU {
                        *phase -= std::f64::consts::TAU;
                    }
                    *remaining -= 1;
                    value
                } else {
                    0
                };
                let start = frame * stride;
                samples[start..start + 2].copy_from_slice(&sample.to_le_bytes());
                samples[start + 2..start + 4].copy_from_slice(&sample.to_le_bytes());
            }
        })
        .register()?;
    let mut tone_info = pw::spa::param::audio::AudioInfoRaw::new();
    tone_info.set_format(pw::spa::param::audio::AudioFormat::S16LE);
    tone_info.set_rate(TONE_RATE);
    tone_info.set_channels(TONE_CHANNELS);
    let mut tone_position = [0; pw::spa::param::audio::MAX_CHANNELS];
    tone_position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    tone_position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    tone_info.set_position(tone_position);
    let tone_format = PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(Object {
            type_: pw::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pw::spa::sys::SPA_PARAM_EnumFormat,
            properties: tone_info.into(),
        }),
    )?
    .0
    .into_inner();
    let mut tone_params = [Pod::from_bytes(&tone_format)
        .ok_or_else(|| anyhow::anyhow!("could not create PipeWire tone format"))?];
    tone_stream.connect(
        pw::spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut tone_params,
    )?;

    // Mute state lives on the main loop thread; the portal thread only sends bools.
    let node: Rc<RefCell<Option<pw::node::Node>>> = Rc::new(RefCell::new(None));
    let muted = Rc::new(Cell::new(true));

    let registry = core.get_registry_rc()?;
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.clone();
            let node = Rc::clone(&node);
            let muted = Rc::clone(&muted);
            move |global| {
                let is_ours = global.type_ == ObjectType::Node
                    && global
                        .props
                        .and_then(|p| p.get("node.name"))
                        .is_some_and(|name| name == SOURCE_NAME);
                if is_ours {
                    if let Ok(bound) = registry.bind::<pw::node::Node, _>(global) {
                        apply_mute(&bound, muted.get());
                        *node.borrow_mut() = Some(bound);
                    }
                }
            }
        })
        .register();

    let (mute_tx, mute_rx) = pw::channel::channel::<bool>();
    let _mute_rx = mute_rx.attach(mainloop.loop_(), {
        let node = Rc::clone(&node);
        let muted = Rc::clone(&muted);
        move |value| {
            let changed = muted.get() != value;
            muted.set(value);
            if let Some(node) = node.borrow().as_ref() {
                apply_mute(node, value);
            }
            if changed {
                tone_request.set(Some(!value));
            }
        }
    });

    thread::Builder::new()
        .name("global-shortcuts-portal".to_owned())
        .spawn(move || {
            if let Err(error) = run_global_shortcuts(mute_tx) {
                eprintln!("Global Shortcuts portal unavailable: {error:#}");
            }
        })?;

    println!(
        "GlobalPTT loopback running muted. Select GlobalPTT Virtual Microphone as the microphone in Discord. Set GLOBALPTT_MIC to choose a source."
    );
    mainloop.run();
    Ok(())
}
