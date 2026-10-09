fn main(){ let mut i=0u64; loop { i=i.wrapping_add(1); if i==u64::MAX {break;} } }
