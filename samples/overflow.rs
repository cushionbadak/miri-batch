use std::hint::black_box;
fn main() { let x: u8 = black_box(255); let y = x + black_box(1); println!("y={y}"); debug_assert!(false, "debug assertions are ON"); }
