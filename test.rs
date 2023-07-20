struct Foo {
  name: String
}

fn bar(arg: Foo) {
  // do something with foo
}

fn main() {
  let foo = Foo { name: "foo".into() };

  bar(foo);

  println!("foo name: {}", foo.name);
}