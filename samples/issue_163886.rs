use std::fmt::Debug;

trait Tr {
    type Assoc: Debug;
    fn get(self) -> Self::Assoc;
}
impl<'q> Tr for &'q u8 {
    type Assoc = &'q u8;
    fn get(self) -> &'q u8 {
        self
    }
}

fn make<'a, 'b, T: Tr>(t: T) -> impl Debug + 'a + 'b
where
    T::Assoc: 'a + 'b,
{
    t.get()
}

fn show<T: Debug>(t: T, r: &Vec<u8>) {
    println!("escaped = {t:?}, reused = {r:?}");
}

fn main() {
    let escaped;
    {
        let v: Vec<u8> = vec![42];
        escaped = make(&v[0]);
        //~^ ERROR `v` does not live long enough
    }
    let reused: Vec<u8> = vec![7];
    show(escaped, &reused);
}