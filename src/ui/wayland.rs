use sctk::reexports::{
  client::{
    Connection, Dispatch, QueueHandle,
    globals::GlobalListContents,
    protocol::wl_registry::{self, WlRegistry},
  },
  protocols::xdg::foreign::zv2::client::{
    zxdg_imported_v2::{self, ZxdgImportedV2},
    zxdg_importer_v2::{self, ZxdgImporterV2},
  },
};

pub struct WaylandState;

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

impl Dispatch<ZxdgImportedV2, ()> for WaylandState {
  fn event(
    _: &mut Self,
    _: &ZxdgImportedV2,
    _: zxdg_imported_v2::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Self>,
  ) {
  }
}
