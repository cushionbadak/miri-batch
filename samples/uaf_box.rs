fn main(){ let b = Box::new(5); let p = Box::into_raw(b); unsafe { drop(Box::from_raw(p)); println!("{}", *p); } }
