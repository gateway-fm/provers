fn main() {
    let erigon_bytes = include_bytes!("./cdk-range");
    let erigon_agg = include_bytes!("./cdk-aggregation");

    color_eyre::install().unwrap();
    prover_elf_utils::ElfInfo::writing_to("vkeys_raw.rs")
        // Verification keys for aggregation proof
        .module("aggregation", erigon_agg)
        .emit_vkey_bytes()
        .emit_vkey_hash()
        .finish()
        // Verification keys for range proof
        .module("range", erigon_bytes)
        .emit_vkey_bytes()
        .emit_vkey_hash()
        .emit_vkey_commitment()
        .finish();
}
