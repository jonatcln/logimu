use crate::impl_dyn;
use super::simulator;
use super::simulator::{Component, InputType, OutputType, ir::IrOp, Graph, GraphNodeHandle, GraphIter, NexusHandle, Port};

use core::fmt;
use core::mem;
use core::ops::{Add, Mul};
use serde::{Serialize, Serializer, Deserialize, Deserializer};
use serde::ser::{SerializeSeq, SerializeTuple, SerializeStruct};
use serde::de;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
	pub x: u16,
	pub y: u16,
}

impl Point {
	pub const MIN: Self = Self { x: u16::MIN, y: u16::MIN };
	pub const MAX: Self = Self { x: u16::MAX, y: u16::MAX };

	pub const fn new(x: u16, y: u16) -> Self {
		Self { x, y }
	}
}

#[derive(Clone, Copy, Debug)]
pub struct PointOffset {
	pub x: i8,
	pub y: i8,
}

impl PointOffset {
	pub const fn new(x: i8, y: i8) -> Self {
		Self { x, y }
	}
}

impl Add<PointOffset> for Point {
	type Output = Option<Self>;

	fn add(self, rhs: PointOffset) -> Self::Output {
		let x = i32::from(self.x) + i32::from(rhs.x);
		let y = i32::from(self.y) + i32::from(rhs.y);
		x.try_into().and_then(|x| y.try_into().map(|y| Self { x, y })).ok()
	}
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Direction {
	Right,
	Down,
	Left,
	Up,
}

impl Direction {
	pub fn rotate_clockwise(self) -> Self {
		match self {
			Self::Right => Self::Down,
			Self::Down => Self::Left,
			Self::Left => Self::Up,
			Self::Up => Self::Right,
		}
	}
}

impl Mul<PointOffset> for Direction {
	type Output = Option<PointOffset>;

	fn mul(self, rhs: PointOffset) -> Self::Output {
		let (x, y) = match self {
			Self::Right => (rhs.x, rhs.y),
			Self::Down => (rhs.y.checked_neg()?, rhs.x),
			Self::Left => (rhs.x.checked_neg()?, rhs.y.checked_neg()?),
			Self::Up => (rhs.y, rhs.x.checked_neg()?),
		};
		Some(PointOffset { x, y })
	}
}

#[derive(Clone, Copy, Debug)]
pub struct Aabb {
	min: Point,
	max: Point,
}

impl Aabb {
	pub const ALL: Self = Self { min: Point::MIN, max: Point::MAX };

	/// Create a new AABB containing two points as tightly as possible.
	pub fn new(p1: Point, p2: Point) -> Self {
		Self {
			min: Point { x: p1.x.min(p2.x), y: p1.y.min(p2.y) },
			max: Point { x: p1.x.max(p2.x), y: p1.y.max(p2.y) },
		}
	}

	/// Check if this AABB contains a point.
	pub fn intersect_point(&self, p: Point) -> bool {
		self.min.x <= p.x && p.x <= self.max.x && self.min.y <= p.y && p.y <= self.max.y
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wire {
	pub from: Point,
	pub to: Point,
}

impl Wire {
	pub fn new(from: Point, to: Point) -> Self {
		Self { from, to }
	}

	/// Check if this wire intersects with a point.
	pub fn intersect_point(&self, point: Point) -> bool {
		// Check if the point is inside the AABB of the wire.
		if !self.aabb().intersect_point(point) {
			return false;
		}

		let (x1, y1) = (i32::from(self.from.x), i32::from(self.from.y));
		let (x2, y2) = (i32::from(self.to.x), i32::from(self.to.y));
		let (xp, yp) = (i32::from(point.x), i32::from(point.y));
		// Make start of line (x1, y1) the origin so b = 0
		let (dx, dy) = (x2 - x1, y2 - y1);
		let (dxp, dyp) = (xp - x1, yp - y1);
		// y = ax <=> y = dy / dx * x <=> y * dx = x * dx
		dx * dyp == dy * dxp
	}

	/// Return the AABB enclosing this wire.
	pub fn aabb(&self) -> Aabb {
		Aabb::new(self.from, self.to)
	}
}

/// A component with fixed input & output locations
pub trait CircuitComponent
where
	Self: simulator::Component,
{
	/// All the inputs of this component.
	fn inputs(&self) -> &[PointOffset];

	/// All the outputs of this component.
	fn outputs(&self) -> &[PointOffset];
}

impl_dyn! {
	Component for &dyn CircuitComponent {
		input_count() -> usize;
		input_type(input: usize) -> Option<InputType>;
		output_count() -> usize;
		output_type(output: usize) -> Option<OutputType>;
		generate_ir(inputs: &[usize], outputs: &[usize], out: &mut dyn FnMut(IrOp)) -> ();
	}
}

impl_dyn! {
	CircuitComponent for &dyn CircuitComponent {
		inputs() -> &[PointOffset];
		outputs() -> &[PointOffset];
	}
}

/// A collection of interconnected wires and components.
pub struct Circuit<C>
where
	C: CircuitComponent,
{
	/// A grid is used to speed up intersection lookups.
	///
	/// To save on memory, the grid is split into zones.
	///
	/// - Root zone: 1024x1024 subzones -> 24 MiB memory on 64-bit.
	/// - Subzone: 64x64 points. Memory usage depends on amount of nodes (components & wires) in zone.
	zones: Box<[[Zone; 1024]; 1024]>,
	/// All wires in this circuit.
	wires: Vec<(Wire, NexusHandle)>,
	/// A graph connecting all nodes. Used for IR generation.
	graph: Graph<C, (Point, Direction), Vec<usize>>,
}

/// A single zone in a circuit.
pub struct Zone {
	/// A mapping from point to component or wire.
	///
	/// - If MSB is 0, maps to wire.
	/// - If MSB is 1, maps to component.
	nodes: Vec<usize>,
}

impl<C> Circuit<C>
where
	C: CircuitComponent,
{
	pub fn new() -> Self {
		Self::default()
	}

	pub fn add_wire(&mut self, wire: Wire) {
		let Aabb { min, max } = wire.aabb();
		let index = self.wires.len();

		// Add wire to existing nexus if it connects with one.
		// Otherwise create a new nexus and add the wire to it.
		let mut nexus = None;
		self.intersect_point(wire.from, |i| nexus = Some(self.wires[i].1), |_| todo!());
		self.intersect_point(wire.to, |i| {
			nexus.is_some().then(|| todo!("handle connecting two separate wires with new wire"));
			nexus = Some(self.wires[i].1);
		}, |_| todo!());
		let nexus = nexus.unwrap_or_else(|| self.graph.new_nexus(Vec::new()));
		self.graph.nexus_mut(nexus).unwrap().userdata.push(index);
		self.wires.push((wire, nexus));

		// Check if this wire connects with any components. If so, connect these components
		// to this wire's nexus.
		self.connect_wire(self.wires.len() - 1);

		let (min_x, min_y) = (min.x / 64, min.y / 64);
		// Round down, then count up to max including max so zero-width/height
		// wires are visible.
		let (max_x, max_y) = (max.x / 64, max.y / 64);
		for y in min_y..=max_y {
			for x in min_x..=max_x {
				self.zones[usize::from(y)][usize::from(x)].add_wire(index);
			}
		}
	}

	pub fn wires(&self, aabb: Aabb) -> WireIter<C> {
		WireIter {
			circuit: self,
			aabb,
			index: 0,
		}
	}

	pub fn add_component(&mut self, component: C, position: Point, direction: Direction) -> usize {
		// Add to graph
		let handle = self.graph.add(component, (position, direction));
		assert_eq!(handle.into_raw() & (1 << mem::size_of_val(&handle.into_raw())), 0);

		// TODO add to zones. This requires per component AABBs.
		handle.into_raw()
	}

	pub fn components(&self, aabb: Aabb) -> ComponentIter<C> {
		ComponentIter {
			iter: self.graph.nodes(),
			circuit: self,
			aabb,
			index: 0,
		}
	}

	pub fn generate_ir(&mut self) -> (Vec<IrOp>, usize) {
		self.connect_wire(usize::MAX);

		let (ir, mem_size) = self.graph.generate_ir();

		(ir, mem_size)
	}

	fn find_ports_at_internal<'a, F, G>(&'a self, pos: Point, mut in_callback: F, mut out_callback: G)
	where
		F: FnMut(GraphNodeHandle, usize),
		G: FnMut(GraphNodeHandle, usize),
	{
		//self.intersect_zone(position).find_ports_at(self, position, in_callback, out_callback);
		for (c, h, &(p, d)) in self.graph.nodes() {
			for (i, &inp) in c.inputs().iter().enumerate() {
				(d * inp)
					.and_then(|inp| p + inp)
					.map(|inp| (inp == pos).then(|| in_callback(h, i)));
			}
			for (i, &outp) in c.outputs().iter().enumerate() {
				(d * outp)
					.and_then(|outp| p + outp)
					.map(|outp| (outp == pos).then(|| out_callback(h, i)));
			}
		}
	}

	fn intersect_zone<'a>(&'a self, position: Point) -> &'a Zone {
		let (x, y) = (usize::from(position.x) / 64, usize::from(position.y) / 64);
		&self.zones[y][x]
	}

	fn intersect_point(&self, position: Point, wire_callback: impl FnMut(usize), component_callback: impl FnMut(usize)) {
		self.intersect_zone(position).intersect_point(self, position, wire_callback, component_callback);
	}

	fn connect_wire(&mut self, wire: usize) {
		// TODO iterating all wires is wasteful.
		// Connect components using wire information
		for (w, nexus) in self.wires.iter() {
			// TODO handle overlapping ports (i.e. ports without wire)
			for p in [w.from, w.to].iter() {
				let (mut inp, mut outp) = (None, None);
				self.find_ports_at_internal(*p, |c, i| inp = Some((c, i)), |c, i| outp = Some((c, i)));
				if let Some((node, port)) = inp {
					self.graph.connect(Port::Input { node, port }, *nexus).unwrap();
				}
				if let Some((node, port)) = outp {
					self.graph.connect(Port::Output { node, port }, *nexus).unwrap();
				}
			}
		}
	}
}

impl<C> Default for Circuit<C>
where
	C: CircuitComponent,
{
	fn default() -> Self {
		use core::mem::MaybeUninit;
		let zones = unsafe {
			// SAFETY: nested MaybeUninits to single MaybeUninit still indicates the underlying
			// memory is unitialized.
			let mut b: Box<[[MaybeUninit<Zone>; 1024]; 1024]> = Box::new_uninit().assume_init();
			b
				.iter_mut()
				.for_each(|r| r.iter_mut().for_each(|e| { e.write(Zone { nodes: Vec::new() }); }));
			// SAFETY: All elements have been initialized and we're simply casting each element
			// from MaybeUninit<Zone> to Zone.
			core::mem::transmute(b)
		};
		Self {
			zones,
			wires: Vec::new(),
			graph: Graph::new(),
		}
	}
}

impl<C> Serialize for Circuit<C>
where
	C: CircuitComponent + Serialize,
{
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let mut circuit = serializer.serialize_struct(stringify!(Circuit), 2)?;
		// TODO avoid redundant box
		circuit.serialize_field("wires", &self.wires.iter().map(|(w, _)| w).collect::<Box<_>>())?;
		circuit.serialize_field(
			"components",
			&self.graph.nodes().map(|(c, _, (p, d))| (c, p, d)).collect::<Box<_>>(),
		)?;
		circuit.end()
	}
}

impl<'a, C> Deserialize<'a> for Circuit<C>
where
	C: CircuitComponent + Deserialize<'a>,
{
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'a>,
	{
		#[derive(Deserialize)]
		#[serde(field_identifier, rename_all = "lowercase")]
		enum Field { Wires, Components }

		struct CircuitVisitor<C>(core::marker::PhantomData<C>);

		impl<'a, C> de::Visitor<'a> for CircuitVisitor<C>
		where
			C: CircuitComponent + Deserialize<'a>,
		{
			type Value = Circuit<C>;
			
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct Circuit")
            }


            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: de::MapAccess<'a>,
            {
				let mut s = Circuit::default();
				let (mut handled_wires, mut handled_components) = (false, false);

				while let Some(key) = map.next_key()? {
					match key {
						Field::Wires => {
							if handled_wires {
								return Err(de::Error::duplicate_field("wires"));
							}
							handled_wires = true;
							map.next_value::<Vec<Wire>>()?.into_iter().for_each(|w| s.add_wire(w));
						},
						Field::Components => {
							if handled_components {
								return Err(de::Error::duplicate_field("wires"));
							}
							handled_components = true;
							for (c, p, d) in map.next_value::<Vec<(C, Point, Direction)>>()? {
								s.add_component(c, p, d);
							}
						},
					}
				}

                Ok(s)
            }
		}

		deserializer.deserialize_struct(
			stringify!(Circuit),
			&["wires", "components"],
			CircuitVisitor(core::marker::PhantomData)
		)
	}
}

impl Zone {
	const COMPONENT_FLAG: usize = 1 << (mem::size_of::<usize>() - 1);

	/// Get all wires and components at a given point.
	fn intersect_point<C>(
		&self,
		circuit: &Circuit<C>,
		position: Point,
		mut wire_callback: impl FnMut(usize),
		mut component_callback: impl FnMut(usize),
	)
	where
		C: CircuitComponent,
	{
		for &n in self.nodes.iter() {
			if n & Self::COMPONENT_FLAG == 0 {
				// Wire
				circuit.wires[n].0.intersect_point(position).then(|| wire_callback(n));
			} else {
				// Component
				let n = n ^ Self::COMPONENT_FLAG;
				todo!();
			}
		}
	}

	fn add_wire(&mut self, index: usize) {
		assert_eq!(index & Self::COMPONENT_FLAG, 0);
		self.nodes.push(index);
	}

	fn find_ports_at<'a, F, C>(&self, circuit: &'a Circuit<C>, position: Point, mut in_callback: F, mut out_callback: F)
	where
		F: FnMut(&'a C, usize),
		C: CircuitComponent,
	{
		todo!()
	}
}

pub struct WireIter<'a, C>
where
	C: CircuitComponent,
{
	circuit: &'a Circuit<C>,
	aabb: Aabb,
	index: usize,
}

impl<'a, C> Iterator for WireIter<'a, C>
where
	C: CircuitComponent,
{
	type Item = &'a Wire;

	fn next(&mut self) -> Option<Self::Item> {
		while let Some((w, _)) = self.circuit.wires.get(self.index) {
			self.index += 1;
			if self.aabb.intersect_point(w.from) || self.aabb.intersect_point(w.to) {
				return Some(w);
			}
		}
		None
	}
}

pub struct ComponentIter<'a, C>
where
	C: CircuitComponent,
{
	// TODO avoid iter, use zones
	iter: GraphIter<'a, C, (Point, Direction), Vec<usize>>,
	circuit: &'a Circuit<C>,
	aabb: Aabb,
	index: usize,
}

impl<'a, C> Iterator for ComponentIter<'a, C>
where
	C: CircuitComponent,
{
	type Item = (&'a C, Point, Direction);

	fn next(&mut self) -> Option<Self::Item> {
		// TODO check AABBs.
		while let Some((c, _, &(p, d))) = self.iter.next() {
			self.index += 1;
			return Some((c, p, d));
		}
		None
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use core::num::NonZeroU8;
	use simulator::{In, Out, AndGate as And, OrGate as Or, NotGate as Not, XorGate as Xor, NonZeroOneU8};


	/// ```
	/// i0 --+-------v
	///      |      AND --> NOT
	/// i1 --|--+----^       |
	///      |  |            v
	///      +--|----v      AND --> o0
	///      |  |    OR -----^
	///      |  +----^
	///      |  |
	///      +--|----v
	///         |   XOR ----------> o1
	///         +----^
	/// ```
	///
	/// NOT:
	/// ```
	/// I -> *0* -> O
	/// ```
	///
	/// AND/OR/XOR:
	/// ```
	/// I1 -> *--
	///       -0* -> O
	/// I2 -> *--
	/// ```
	#[test]
	fn manual_xor() {

		let mut circuit = Box::<Circuit<&dyn CircuitComponent>>::default();

		let bits = NonZeroU8::new(1).unwrap();
		let inputs = NonZeroOneU8::new(2).unwrap();
		let i0 = In::new(bits, 0);
		let i1 = In::new(bits, 1);
		let l0 = And::new(inputs, bits);
		let l1 = Not::new(bits);
		let r0 = Or::new(inputs, bits);
		let lr = And::new(inputs, bits);
		let o0 = Out::new(bits, 0);
		let cp = Xor::new(inputs, bits);
		let o1 = Out::new(bits, 1);

		// Inputs
		circuit.add_component(&i0, Point::new(0, 0), Direction::Right);
		circuit.add_component(&i1, Point::new(0, 4), Direction::Right);

		// Connect inputs to AND
		circuit.add_wire(Wire::new(Point::new(0, 0), Point::new(3, 0)));
		circuit.add_wire(Wire::new(Point::new(0, 4), Point::new(3, 2)));
		// Place AND and NOT
		circuit.add_component(&l0, Point::new(4, 1), Direction::Right);
		circuit.add_component(&l1, Point::new(8, 0), Direction::Right);
		// Connect AND to NOT
		circuit.add_wire(Wire::new(Point::new(5, 1), Point::new(7, 0)));

		// Place OR
		circuit.add_component(&r0, Point::new(4, 4), Direction::Right);
		// Connect inputs to OR
		circuit.add_wire(Wire::new(Point::new(0, 0), Point::new(3, 3)));
		circuit.add_wire(Wire::new(Point::new(0, 4), Point::new(3, 5)));
		
		// Connect AND & OR to AND and connect AND to output
		circuit.add_wire(Wire::new(Point::new(9, 0), Point::new(11, 0)));
		circuit.add_wire(Wire::new(Point::new(5, 4), Point::new(11, 2)));
		circuit.add_wire(Wire::new(Point::new(13, 1), Point::new(16, 0)));
		// Place AND and output
		circuit.add_component(&lr, Point::new(12, 1), Direction::Right);
		circuit.add_component(&o0, Point::new(16, 0), Direction::Right);

		// Place XOR and output
		circuit.add_component(&cp, Point::new(4, 8), Direction::Right);
		circuit.add_component(&o1, Point::new(16, 8), Direction::Right);
		// Connect inputs to XOR and XOR to output
		circuit.add_wire(Wire::new(Point::new(0, 0), Point::new(3, 7)));
		circuit.add_wire(Wire::new(Point::new(0, 4), Point::new(3, 9)));
		circuit.add_wire(Wire::new(Point::new(5, 8), Point::new(16, 8)));

		let (ir, _) = circuit.generate_ir();
		let (a, b) = (0b1100, 0b0110);
		let mut out = [0; 2];
		simulator::ir::interpreter::run(&ir, &mut [0; 32], &[a, b], &mut out);
		assert_eq!(out, [a ^ b; 2]);
	}

	#[test]
	fn serde() {
		use serde_test::*;

		#[typetag::serde]
		trait T: CircuitComponent {}

		impl_dyn! {
			Component for &dyn T {
				input_count() -> usize;
				input_type(input: usize) -> Option<InputType>;
				output_count() -> usize;
				output_type(output: usize) -> Option<OutputType>;
				generate_ir(inputs: &[usize], outputs: &[usize], out: &mut dyn FnMut(IrOp)) -> ();
			}
		}

		impl_dyn! {
			CircuitComponent for &dyn T {
				inputs() -> &[PointOffset];
				outputs() -> &[PointOffset];
			}
		}

		#[typetag::serde]
		impl T for In {}

		let mut c = Box::<Circuit<&dyn T>>::default();

		c.add_wire(Wire::new(Point::new(1, 1), Point::new(4, 4)));
		let n = In::new(NonZeroU8::new(3).unwrap(), 6);
		c.add_component(&n, Point::new(1, 1), Direction::Up);

		assert_ser_tokens(&c, &[
			Token::Struct { len: 2, name: stringify!(Circuit) },
			Token::Str("wires"),
			Token::Seq { len: Some(1) },
			Token::Struct { len: 2, name: stringify!(Wire) },
			Token::Str("from"),
			Token::Struct { len: 2, name: stringify!(Point) },
			Token::Str("x"),
			Token::U16(1),
			Token::Str("y"),
			Token::U16(1),
			Token::StructEnd,
			Token::Str("to"),
			Token::Struct { len: 2, name: stringify!(Point) },
			Token::Str("x"),
			Token::U16(4),
			Token::Str("y"),
			Token::U16(4),
			Token::StructEnd,
			Token::StructEnd,
			Token::SeqEnd,
			Token::Str("components"),
			Token::Seq { len: Some(1) },
			Token::Tuple { len: 3 },
			Token::Map { len: Some(1) },
			Token::Str("In"),
			Token::Struct { name: "In", len: 2 },
			Token::Str("bits"),
			Token::U8(3),
			Token::Str("index"),
			Token::U64(6),
			Token::StructEnd,
			Token::MapEnd,
			Token::Struct { name: "Point", len: 2 },
			Token::Str("x"),
			Token::U16(1),
			Token::Str("y"),
			Token::U16(1),
			Token::StructEnd,
			Token::UnitVariant { name: "Direction", variant: "Up" },
			Token::TupleEnd,
			Token::SeqEnd,
			Token::StructEnd,
		]);
	}
}
