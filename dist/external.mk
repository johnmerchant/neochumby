include $(sort $(wildcard $(BR2_EXTERNAL_NEOCHUMBY_PATH)/package/*/*.mk))

# The kernel's Rust, from dist/Containerfile, by absolute path: Buildroot puts
# its own host/bin, with the Rust that builds ncinit, first on the PATH. 5.9.16
# has no Rust and ignores these.
LINUX_MAKE_FLAGS += \
	RUSTC=/opt/rust/bin/rustc \
	HOSTRUSTC=/opt/rust/bin/rustc \
	RUSTDOC=/opt/rust/bin/rustdoc \
	RUSTFMT=/opt/rust/bin/rustfmt \
	CLIPPY_DRIVER=/opt/rust/bin/clippy-driver \
	BINDGEN=/opt/rust/bin/bindgen

# Buildroot's HOSTCC holds the compiler and its flags, but the kernel gives
# rustc -Clinker=$(HOSTCC) unquoted, so rustc would read those flags as its
# own. The compiler alone, then, with its flags where the kernel looks.
LINUX_MAKE_FLAGS += \
	HOSTCC="$(HOSTCC_NOCCACHE)" \
	HOSTCFLAGS="$(subst -I/,-isystem /,$(subst -I /,-isystem /,$(HOST_CFLAGS)))" \
	HOSTLDFLAGS="$(HOST_LDFLAGS)"
