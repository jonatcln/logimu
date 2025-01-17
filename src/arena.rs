use core::iter::Enumerate;
use core::mem;
use core::ops::{Index, IndexMut};
use core::slice;

#[derive(Debug)]
enum Entry<T> {
	Free { next: Option<usize> },
	Occupied { value: T },
}

impl<T> Entry<T> {
	fn as_occupied(&self) -> Option<&T> {
		match self {
			Self::Occupied { value } => Some(value),
			_ => None,
		}
	}

	fn as_occupied_mut(&mut self) -> Option<&mut T> {
		match self {
			Self::Occupied { value } => Some(value),
			_ => None,
		}
	}
}

pub struct Arena<T> {
	list: Vec<Entry<T>>,
	free: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(usize);

impl Handle {
	pub fn index(self) -> usize {
		self.0
	}
}

impl<T> Arena<T> {
	pub fn get(&self, handle: Handle) -> Option<&T> {
		self.list.get(handle.0).and_then(Entry::as_occupied)
	}

	pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
		self.list.get_mut(handle.0).and_then(Entry::as_occupied_mut)
	}

	pub fn insert(&mut self, element: T) -> Handle {
		self.insert_with(|_| element)
	}

	pub fn insert_with(&mut self, f: impl FnOnce(Handle) -> T) -> Handle {
		if let Some(free) = self.free {
			if let Some(Entry::Free { next }) = self.list.get(free) {
				let handle = Handle(free);
				self.free = *next;
				self.list[free] = Entry::Occupied { value: f(handle) };
				handle
			} else {
				unreachable!()
			}
		} else {
			let handle = Handle(self.list.len());
			self.list.push(Entry::Occupied { value: f(handle) });
			handle
		}
	}

	pub fn remove(&mut self, handle: Handle) -> Option<T> {
		self.list.get_mut(handle.0).and_then(|e| {
			let next = self.free.replace(handle.0);
			match mem::replace(e, Entry::Free { next }) {
				Entry::Occupied { value } => Some(value),
				free => {
					*e = free;
					None
				}
			}
		})
	}

	pub fn iter(&self) -> Iter<T> {
		Iter { iter: self.list.iter().enumerate() }
	}

	#[allow(dead_code)]
	pub fn iter_mut(&mut self) -> IterMut<T> {
		IterMut { iter: self.list.iter_mut().enumerate() }
	}
}

impl<T> Default for Arena<T> {
	fn default() -> Self {
		Self { list: Default::default(), free: None }
	}
}

impl<T> Index<Handle> for Arena<T> {
	type Output = T;

	fn index(&self, index: Handle) -> &Self::Output {
		self.get(index).expect("invalid handle")
	}
}

impl<T> IndexMut<Handle> for Arena<T> {
	fn index_mut(&mut self, index: Handle) -> &mut Self::Output {
		self.get_mut(index).expect("invalid handle")
	}
}

pub struct Iter<'a, T> {
	iter: Enumerate<slice::Iter<'a, Entry<T>>>,
}

impl<'a, T> Iterator for Iter<'a, T> {
	type Item = (Handle, &'a T);

	fn next(&mut self) -> Option<Self::Item> {
		while let Some((i, e)) = self.iter.next() {
			if let Some(e) = e.as_occupied() {
				return Some((Handle(i), e));
			}
		}
		None
	}
}

pub struct IterMut<'a, T> {
	iter: Enumerate<slice::IterMut<'a, Entry<T>>>,
}

impl<'a, T> Iterator for IterMut<'a, T> {
	type Item = (Handle, &'a mut T);

	fn next(&mut self) -> Option<Self::Item> {
		while let Some((i, e)) = self.iter.next() {
			if let Some(e) = e.as_occupied_mut() {
				return Some((Handle(i), e));
			}
		}
		None
	}
}
