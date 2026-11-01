################################################################################
#
# neochumby-init: ncinit, the image's /init
#
################################################################################

# crates/neochumby-init, a workspace of its own, and the neochumby-sys it
# uses, built where they are in this checkout (dist/build mounts it):
# neochumby-sys takes its edition and version from the checkout's root
# Cargo.toml, which a copy of crates/ alone wouldn't have. Buildroot's copy
# of the crate is only its source of record; dist/build rebuilds it every
# run, as Buildroot can't see changes in the checkout.
NEOCHUMBY_INIT_VERSION = local
NEOCHUMBY_INIT_SITE = $(BR2_EXTERNAL_NEOCHUMBY_PATH)/../crates/neochumby-init
NEOCHUMBY_INIT_SITE_METHOD = local
NEOCHUMBY_INIT_LICENSE = MIT
NEOCHUMBY_INIT_DEPENDENCIES = host-rustc
NEOCHUMBY_INIT_CRATE = $(BR2_EXTERNAL_NEOCHUMBY_PATH)/../crates/neochumby-init

# Buildroot's rustc and cross gcc, as for its own cargo packages, but the
# crate's own release profile (small: it's in the initramfs) over the one
# Buildroot's options would give. Static, though the image's other
# programs needn't be: process 1 must start (and start a rescue shell)
# whatever state libc.so is in.
#
# Rust's std for musl links libunwind, which Buildroot's musl toolchain
# hasn't got. Rust ships one for the target, beside its own musl's libc.a
# and crt files: copy just libunwind.a where the linker looks, so the
# rest still comes from Buildroot's musl.
NEOCHUMBY_INIT_RUSTLIB = $(HOST_DIR)/lib/rustlib/$(RUSTC_TARGET_NAME)/lib/self-contained

define NEOCHUMBY_INIT_BUILD_CMDS
	$(INSTALL) -D -m 0644 $(NEOCHUMBY_INIT_RUSTLIB)/libunwind.a $(@D)/unwind/libunwind.a
	cd $(NEOCHUMBY_INIT_CRATE) && $(TARGET_MAKE_ENV) $(PKG_CARGO_ENV) \
		CARGO_TARGET_DIR=$(@D)/target \
		CARGO_PROFILE_RELEASE_OPT_LEVEL=s \
		CARGO_PROFILE_RELEASE_LTO=true \
		CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
		CARGO_TARGET_$(call UPPERCASE,$(RUSTC_TARGET_NAME))_RUSTFLAGS="-C target-feature=+crt-static -L native=$(@D)/unwind" \
		cargo build --release --locked
endef

define NEOCHUMBY_INIT_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 \
		$(@D)/target/$(RUSTC_TARGET_NAME)/release/ncinit \
		$(TARGET_DIR)/init
endef

$(eval $(generic-package))
