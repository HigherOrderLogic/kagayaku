use std::collections::HashMap;

use anyhow::Result as AnyResult;
use iced::window;
use sctk::reexports::{
  calloop::{
    EventLoop,
    channel::{Channel, Event as ChannelEvent},
  },
  calloop_wayland_source::WaylandSource,
  client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
      wl_registry::{self, WlRegistry},
      wl_surface::WlSurface,
    },
  },
  protocols::xdg::foreign::zv2::client::{
    zxdg_imported_v2::{self, ZxdgImportedV2},
    zxdg_importer_v2::{self, ZxdgImporterV2},
  },
};
use smol::channel::Sender as SmolSender;

use crate::ui::WaylandMessage;

pub enum ToWaylandMessage {
  SetParent(WlSurface, String, window::Id),
}

struct WaylandState {
  registry: RegistryState,
  to_ui: SmolSender<WaylandMessage>,
  qh: QueueHandle<WaylandState>,
  importer: Option<ZxdgImporterV2>,
  imported: HashMap<window::Id, ZxdgImportedV2>,
}

impl Dispatch<WlRegistry, GlobalListContents> for WaylandState {
  fn event(
    _: &mut WaylandState,
    _: &WlRegistry,
    _: wl_registry::Event,
    _: &GlobalListContents,
    _: &Connection,
    _: &QueueHandle<WaylandState>,
  ) {
  }
}

impl Dispatch<ZxdgImporterV2, ()> for WaylandState {
  fn event(
    _: &mut Self,
    _: &ZxdgImporterV2,
    _: zxdg_importer_v2::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}

impl Dispatch<ZxdgImportedV2, window::Id> for WaylandState {
  fn event(
    state: &mut Self,
    proxy: &ZxdgImportedV2,
    event: zxdg_imported_v2::Event,
    window_id: &window::Id,
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
    if let zxdg_imported_v2::Event::Destroyed = event {
      state.imported.remove(window_id);
      proxy.destroy();
      if let Err(e) = state.to_ui.try_send(WaylandMessage::ParentDestroyed(*window_id)) {
        tracing::warn!("failed to send message to UI thread: {}", e);
      }
    }
  }
}

pub fn run_wayland_loop(
  conn: Connection,
  rx: Channel<ToWaylandMessage>,
  to_ui: SmolSender<WaylandMessage>,
) -> AnyResult<()> {
  let (globals, event_queue) = registry_queue_init(&conn)?;
  let qh = event_queue.handle();
  let mut event_loop = EventLoop::try_new()?;
  let loop_handle = event_loop.handle();
  let signal = event_loop.get_signal();
  let importer = globals.bind(&qh, 1..=1, ()).ok();
  let mut state = WaylandState {
    to_ui,
    qh,
    importer,
    imported: HashMap::new(),
  };

  loop_handle
    .insert_source(rx, move |e, _, state: &mut WaylandState| match e {
      ChannelEvent::Msg(ToWaylandMessage::SetParent(s, p, wid)) => {
        if let Some(i) = state.importer.as_ref() {
          let imported = i.import_toplevel(p, &state.qh, wid);
          imported.set_parent_of(&s);
          state.imported.insert(wid, imported);
        }
      }
      ChannelEvent::Closed => {
        for (_, i) in state.imported.drain() {
          i.destroy();
        }
        if let Some(i) = state.importer.take() {
          i.destroy();
        }
        signal.stop();
      }
    })
    .map_err(|e| e.error)?;

  WaylandSource::new(conn, event_queue)
    .insert(loop_handle)
    .map_err(|e| e.error)?;

  event_loop.run(None, &mut state, |_| {}).map_err(|e| e.into())
}
