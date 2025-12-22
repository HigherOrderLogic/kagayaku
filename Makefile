prefix ?= /usr
libexecdir = $(prefix)/libexec
libdir = $(prefix)/lib
datadir = $(prefix)/share

CARGO_TARGET_DIR ?= target

install:
	install -Dm0755 $(CARGO_TARGET_DIR)/release/kagayaku $(DESTDIR)$(libexecdir)/kagayaku
	sed "s|@libexecdir@|$(libexecdir)|" resources/org.freedesktop.impl.portal.desktop.kagayaku.service.in > resources/org.freedesktop.impl.portal.desktop.kagayaku.service
	install -Dm0644 resources/org.freedesktop.impl.portal.desktop.kagayaku.service $(DESTDIR)$(datadir)/dbus-1/services/org.freedesktop.impl.portal.desktop.kagayaku.service
	sed "s|@libexecdir@|$(libexecdir)|" resources/xdg-desktop-portal-kagayaku.service.in > resources/xdg-desktop-portal-kagayaku.service
	install -Dm0644 resources/xdg-desktop-portal-kagayaku.service $(DESTDIR)$(libdir)/systemd/user/xdg-desktop-portal-kagayaku.service
	install -Dm0644 resources/kagayaku.portal $(DESTDIR)$(datadir)/xdg-desktop-portal/portals/kagayaku.portal
