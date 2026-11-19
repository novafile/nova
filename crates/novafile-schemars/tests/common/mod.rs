//! Fixtures shared by the integration tests.

/// The contents of a file in the repository's `examples/` directory.
pub fn example(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Every type, constraint and attribute the schema language has.
pub const KITCHEN_SINK: &str = "\
@schema kitchen/sink@0.3.0-beta.1
@root Everything

type Port = u16 @range(1..65535)

type Ratio = rational @range(0..1)

record Everything:
  id: string @key @id(1) @len(1..32)
  any_value: any @id(2)
  nothing: null
  flag: bool = false
  big: int @range(-5..)
  i8: i8
  i16: i16 @range(-100..100)
  i32: i32
  i64: i64
  u8: u8
  u16: Port
  u32: u32
  u64: u64
  ratio: float @range(0..1)
  price: decimal @range(0..1000.5)
  share: Ratio
  name: string @len(..10, utf8)
  blob: bytes @len(..16)
  at: timestamp = @timestamp(\"2026-01-01T00:00:00Z\")
  ttl: duration = @duration(\"1h30m\")
  tags: set<string> @len(1..)
  points: list<(float, float)>
  labels: map<string, int>
  by_id: map<int, string>
  nested: list<map<string, Inner?>>
  maybe: Inner?
  status: Status = !idle
  old?: string @deprecated
  later?: timestamp?

record Inner @open:
  x: float
  y: float

enum Status:
  idle
  busy(u8)
  stopped
  failed(Inner)
";
