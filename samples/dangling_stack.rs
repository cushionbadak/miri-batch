use std::hint::black_box;
fn main() {
    let p: *const i32; { let x = black_box(5); p = &x; }
    let v = unsafe { *p };   // use after scope end (dangling stack pointer)
    println!("{v}");
}
