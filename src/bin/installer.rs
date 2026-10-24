#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![allow(non_snake_case)]

fn main() -> anyhow::Result<()> {
    Pekora_bootstrapper::Run(std::env::args().skip(1).collect(), true)
}
