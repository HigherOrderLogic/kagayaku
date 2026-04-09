use std::{
  env::var_os,
  fs::{self, File, read_dir},
  path::PathBuf,
  slice,
};

use anyhow::{Context, Error as AnyError};
use regex::Regex;
use zbus_xml::Node;
use zbus_xmlgen::write_interfaces;

fn proxy_defaults(interface: &str) -> Option<(&'static str, Option<&'static str>)> {
  match interface {
    "org.gnome.Mutter.DisplayConfig" => Some((
      "org.gnome.Mutter.DisplayConfig",
      Some("/org/gnome/Mutter/DisplayConfig"),
    )),
    "org.gnome.Mutter.ScreenCast" => Some(("org.gnome.Mutter.ScreenCast", Some("/org/gnome/Mutter/ScreenCast"))),
    "org.gnome.Mutter.ScreenCast.Session" | "org.gnome.Mutter.ScreenCast.Stream" => {
      Some(("org.gnome.Mutter.ScreenCast", None))
    }
    "org.gnome.Shell.Introspect" => Some(("org.gnome.Shell.Introspect", Some("/org/gnome/Shell/Introspect"))),
    _ => None,
  }
}

fn apply_proxy_defaults(interface: &str, iface_impl: String) -> String {
  let Some((service, path)) = proxy_defaults(interface) else {
    return iface_impl;
  };

  let new_attr = match path {
    Some(path) => {
      format!("#[proxy(interface = \"{interface}\", default_service = \"{service}\", default_path = \"{path}\")]")
    }
    None => format!("#[proxy(interface = \"{interface}\", default_service = \"{service}\")]"),
  };

  let escaped_interface = regex::escape(interface);
  let proxy_attr_re = Regex::new(&format!(
    r#"(?s)#\[proxy\(\s*interface\s*=\s*"{escaped_interface}"\s*,\s*assume_defaults\s*=\s*true\s*\)\]"#
  ))
  .expect("valid proxy attribute regex");
  proxy_attr_re.replacen(&iface_impl, 1, new_attr).into_owned()
}

fn main() -> Result<(), AnyError> {
  let mut xml_dir = PathBuf::from(var_os("CARGO_MANIFEST_DIR").context("missing CARGO_MANIFEST_DIR")?);
  xml_dir.push("resources");
  xml_dir.push("dbus");

  let mut interfaces_impl = Vec::new();

  for entry in read_dir(xml_dir)? {
    let path = match entry {
      Ok(e) => e.path(),
      Err(_) => continue,
    };
    if !path.is_file() {
      continue;
    }
    let (fdo_standard_ifaces, needed_ifaces): (Vec<_>, Vec<_>) = Node::from_reader(File::open(path.to_path_buf())?)?
      .interfaces()
      .iter()
      .cloned()
      .partition(|i| i.name().starts_with("org.freedesktop.DBus"));

    for iface in needed_ifaces {
      let mod_name = iface.name().as_str().to_lowercase().replace(".", "_");
      let iface_impl = apply_proxy_defaults(
        iface.name().as_str(),
        write_interfaces(
          slice::from_ref(&iface),
          &fdo_standard_ifaces,
          None,
          None,
          path.to_str().context("failed to convert to string")?,
          "build.rs",
          "build.rs",
        )
        .unwrap(),
      );
      interfaces_impl.push(format!("pub mod {} {{ {} }}", mod_name, iface_impl));
    }
  }

  let mut out_file = PathBuf::from(var_os("OUT_DIR").context("missing OUT_DIR")?);
  out_file.push("dbus.rs");

  fs::write(
    out_file,
    interfaces_impl
      .iter()
      .map(|s| s.as_str())
      .collect::<Vec<_>>()
      .join("\n")
      .as_bytes(),
  )?;

  Ok(())
}
