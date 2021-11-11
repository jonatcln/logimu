use super::*;
use core::cell::Cell;
use std::sync::Arc;

/// A component representing read-only memory.
#[derive(Default, Serialize, Deserialize)]
pub struct ReadOnlyMemory {
	contents: Vec<usize>,
	#[serde(skip)]
	cached: Cell<Option<Arc<[usize]>>>,
}

impl ReadOnlyMemory {
	pub fn get(&self, index: usize) -> Option<usize> {
		self.contents.get(index).copied()
	}
}

impl Component for ReadOnlyMemory {
	fn input_count(&self) -> usize {
		1
	}

	fn input_type(&self, input: usize) -> Option<InputType> {
		(input < 1).then(|| InputType { bits: NonZeroU8::new(32).unwrap() })
	}

	fn output_count(&self) -> usize {
		1
	}

	fn output_type(&self, output: usize) -> Option<OutputType> {
		(output < 1).then(|| OutputType { bits: NonZeroU8::new(32).unwrap() })
	}

	fn generate_ir(
		&self,
		inputs: &[usize],
		outputs: &[usize],
		outf: &mut dyn FnMut(IrOp),
		memory_size: usize,
	) -> usize {
		let (address, out) = (inputs[0], outputs[0]);
		if address != usize::MAX && out != usize::MAX {
			let mut cached = self.cached.take();
			let memory = cached
				.get_or_insert_with(|| self.contents.clone().into())
				.clone();
			outf(IrOp::Read { memory, address, out });
			self.cached.set(cached);
		}
		0
	}

	fn properties(&self) -> Box<[Property]> {
		// TODO some 'memory' / 'dialog' property should be used for editing a large amount of
		// data
		let range = i32::MIN.into()..=u32::MAX.into();
		self.contents
			.iter()
			.chain(Some(&0))
			.enumerate()
			.map(|(i, e)| {
				Property::new(
					format!("0x{:03x}", i),
					PropertyValue::Int { value: *e as i64, range: range.clone() },
				)
			})
			.collect()
	}

	fn set_property(&mut self, name: &str, value: SetProperty) -> Result<(), Box<dyn Error>> {
		if !name.starts_with("0x") {
			Err("invalid property")?;
		}
		match (
			usize::from_str_radix(name.split_at(2).1, 16),
			value.as_int(),
		) {
			(Ok(i), Some(v)) if i < self.contents.len() => self.contents[i] = v as usize,
			(Ok(i), Some(v)) if i == self.contents.len() => self.contents.push(v as usize),
			(Ok(_), Some(_)) => Err("address out of range")?,
			(Err(_), ..) => Err("invalid property")?,
			(.., None) => Err("expected integer")?,
		}
		self.cached.set(None);
		Ok(())
	}
}
