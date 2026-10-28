// One group of routines per assembly of libgcc/lib1funcs.S (fp/*.S each
// define the group and include it), as libgcc itself builds them.
const GROUPS: &[&str] = &[
    "arm_addsubsf3",
    "arm_muldivsf3",
    "arm_cmpsf2",
    "arm_unordsf2",
    "arm_fixsfsi",
    "arm_fixunssfsi",
    "arm_negsf2",
    "arm_addsubdf3",
    "arm_muldivdf3",
    "arm_cmpdf2",
    "arm_unorddf2",
    "arm_fixdfsi",
    "arm_fixunsdfsi",
    "arm_truncdfsf2",
    "arm_negdf2",
];

fn main() {
    println!("cargo:rerun-if-changed=fp");
    println!("cargo:rerun-if-changed=libgcc");
    // ARM assembly: on any other target the generic routines stand.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("arm") {
        return;
    }
    let mut build = cc::Build::new();
    for g in GROUPS {
        build.file(format!("fp/{g}.S"));
    }
    build.include("libgcc").compile("neochumby_softfloat");
}
