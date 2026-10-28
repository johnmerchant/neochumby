{
  description = "QEMU with the Chumby Classic machine, for neochumby's emulator";

  # qemu-chumby at a pinned commit; keep `build` in step.
  inputs.qemu-chumby.url = "github:johnmerchant/qemu-chumby/a872dff94227a8e5935f2a7ce93c23c248b7d4e4";

  outputs =
    { qemu-chumby, ... }:
    {
      inherit (qemu-chumby) packages devShells;
    };
}
