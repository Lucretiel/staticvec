#![no_std]
#![allow(incomplete_features)]
#![feature(const_fn)]
#![feature(const_generics)]
#![feature(const_if_match)]
#![feature(const_raw_ptr_to_usize_cast)]
#![feature(core_intrinsics)]
#![feature(doc_cfg)]
#![feature(exact_size_is_empty)]
#![feature(maybe_uninit_extra)]
#![feature(maybe_uninit_ref)]
#![feature(maybe_uninit_uninit_array)]
#![cfg_attr(feature = "std", feature(read_initializer))]
#![feature(slice_partition_dedup)]
#![feature(specialization)]
#![feature(trusted_len)]

pub use crate::iterators::*;
pub use crate::trait_impls::*;
use crate::utils::*;
use core::cmp::{Ord, PartialEq};
use core::intrinsics;
use core::marker::PhantomData;
use core::mem::{self, MaybeUninit};
use core::ops::{Bound::Excluded, Bound::Included, Bound::Unbounded, RangeBounds};
use core::ptr;
use core::slice;

#[cfg(any(feature = "std", rustdoc))]
extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

#[cfg(any(feature = "std", rustdoc))]
use alloc::vec::Vec;

mod iterators;
#[macro_use]
mod macros;
mod trait_impls;
#[doc(hidden)]
pub mod utils;

/// A [`Vec`](alloc::vec::Vec)-like struct (mostly directly API-compatible where it can be)
/// implemented with const generics around an array of fixed `N` capacity.
pub struct StaticVec<T, const N: usize> {
  data: [MaybeUninit<T>; N],
  length: usize,
}

impl<T, const N: usize> StaticVec<T, N> {
  /// Returns a new StaticVec instance.
  #[inline(always)]
  pub fn new() -> Self {
    Self {
      data: Self::new_data(),
      length: 0,
    }
  }

  /// Returns a new StaticVec instance filled with the contents, if any, of a slice reference,
  /// which can be either `&mut` or `&` as if it is `&mut` it will implicitly coerce to `&`.
  /// If the slice has a length greater than the StaticVec's declared capacity,
  /// any contents after that point are ignored.
  /// Locally requires that `T` implements [`Copy`](core::marker::Copy) to avoid soundness issues.
  #[inline]
  pub fn new_from_slice(values: &[T]) -> Self
  where T: Copy {
    let length = values.len().min(N);
    Self {
      data: {
        let mut data = Self::new_data_uninit();
        unsafe {
          values
            .as_ptr()
            .copy_to_nonoverlapping(data.as_mut_ptr() as *mut T, length);
          data.assume_init()
        }
      },
      length,
    }
  }

  /// Returns a new StaticVec instance filled with the contents, if any, of an array.
  /// If the array has a length greater than the StaticVec's declared capacity,
  /// any contents after that point are ignored.
  ///
  /// The `N2` parameter does not need to be provided explicitly, and can be inferred from the array
  /// itself. This function does *not* leak memory, as any ignored extra elements in the source
  /// array are explicitly dropped with [`drop_in_place`](core::ptr::drop_in_place) before
  /// [`forget`](core::mem::forget) is called on it.
  ///
  /// Example usage:
  /// ```
  /// // Same input length as the declared capacity:
  /// let v = StaticVec::<i32, 3>::new_from_array([1, 2, 3]);
  /// assert_eq!(v, [1, 2, 3]);
  /// // Truncated to fit the declared capacity:
  /// let v2 = StaticVec::<i32, 3>::new_from_array([1, 2, 3, 4, 5, 6]);
  /// assert_eq!(v2, [1, 2, 3]);
  /// ```
  /// Note that StaticVec also implements [`From`](std::convert::From) for both slices and static
  /// arrays, which may prove more ergonomic in some cases as it allows for a greater degree of
  /// type inference:
  /// ```
  /// // The StaticVec on the next line is inferred to be of type `StaticVec<&'static str, 4>`.
  /// let v = StaticVec::from(["A", "B", "C", "D"]);
  /// ```
  #[inline]
  pub fn new_from_array<const N2: usize>(mut values: [T; N2]) -> Self {
    if N == N2 {
      Self::from(values)
    } else {
      Self {
        data: {
          unsafe {
            let mut data = Self::new_data_uninit();
            values
              .as_ptr()
              .copy_to_nonoverlapping(data.as_mut_ptr() as *mut T, N2.min(N));
            // Drops any extra values left in the source array, then "forgets it".
            ptr::drop_in_place(values.get_unchecked_mut(N2.min(N)..N2));
            mem::forget(values);
            data.assume_init()
          }
        },
        length: N2.min(N),
      }
    }
  }

  /// Returns a new StaticVec instance filled with the return value of an initializer function.
  /// The length field of the newly created StaticVec will be equal to its capacity.
  ///
  /// Example usage:
  /// ```
  /// let mut i = 0;
  /// let v = StaticVec::<i32, 64>::filled_with(|| { i += 1; i });
  /// assert_eq!(v.len(), 64);
  /// assert_eq!(v[0], 1);
  /// assert_eq!(v[1], 2);
  /// assert_eq!(v[2], 3);
  /// assert_eq!(v[3], 4);
  /// ```
  #[inline]
  pub fn filled_with<F>(mut initializer: F) -> Self
  where F: FnMut() -> T {
    let mut res = Self::new();
    // You might think it would make more sense to use `push_unchecked` here.
    // Originally, I did also! However, as of today (November 19, 2019), doing so
    // both in this function and several others throughout the crate inhibits the ability
    // of `rustc` to fully unroll and autovectorize various constant-bounds loops. If this changes
    // in the future, feel free to open a PR switching out the manual code for `get_unchecked`, if
    // you happen to notice it before I do.
    for i in 0..N {
      unsafe {
        res.data.get_unchecked_mut(i).write(initializer());
        res.length += 1;
      }
    }
    res
  }

  /// Returns a new StaticVec instance filled with the return value of an initializer function.
  /// Unlike for [`filled_with`](crate::StaticVec::filled_with), the initializer function in
  /// this case must take a single usize variable as an input parameter, which will be called
  /// with the current index of the `0..N` loop that
  /// [`filled_with_by_index`](crate::StaticVec::filled_with_by_index) is implemented with
  /// internally. The length field of the newly created StaticVec will be equal to its capacity.
  ///
  /// Example usage:
  /// ```
  /// let v = StaticVec::<usize, 64>::filled_with_by_index(|i| { i + 1 });
  /// assert_eq!(v.len(), 64);
  /// assert_eq!(v[0], 1);
  /// assert_eq!(v[1], 2);
  /// assert_eq!(v[2], 3);
  /// assert_eq!(v[3], 4);
  /// ```
  #[inline]
  pub fn filled_with_by_index<F>(mut initializer: F) -> Self
  where F: FnMut(usize) -> T {
    let mut res = Self::new();
    for i in 0..N {
      unsafe {
        res.data.get_unchecked_mut(i).write(initializer(i));
        res.length += 1;
      }
    }
    res
  }

  /// Returns the current length of the StaticVec.
  /// Just as for a normal [`Vec`](alloc::vec::Vec), this means the number of elements that
  /// have been added to it with [`push`](crate::StaticVec::push),
  /// [`insert`](crate::StaticVec::insert), etc. except in the case that it has been set directly
  /// with the unsafe [`set_len`](crate::StaticVec::set_len) function.
  #[inline(always)]
  pub const fn len(&self) -> usize {
    self.length
  }

  /// Returns the total capacity of the StaticVec.
  /// This is always equivalent to the generic `N` parameter it was declared with,
  /// which determines the fixed size of the backing array.
  #[inline(always)]
  pub const fn capacity(&self) -> usize {
    N
  }

  /// Does the same thing as [`capacity`](crate::StaticVec::capacity), but as an associated
  /// function rather than a method.
  #[inline(always)]
  pub const fn cap() -> usize {
    N
  }

  /// Serves the same purpose as [`capacity`](crate::StaticVec::capacity), but as an associated
  /// constant rather than a method.
  pub const CAPACITY: usize = N;

  /// Returns the remaining capacity of the StaticVec.
  #[inline(always)]
  pub const fn remaining_capacity(&self) -> usize {
    N - self.length
  }

  /// Directly sets the length field of the StaticVec to `new_len`. Useful if you intend
  /// to write to it solely element-wise, but marked unsafe due to how it creates
  /// the potential for reading from uninitialized memory later on.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that `new_len` is less than or equal to the StaticVec's
  /// constant `N` parameter, and that the range of elements covered by a length of `new_len` is
  /// actually initialized. Failure to do so will almost certainly result in undefined behavior.
  #[inline(always)]
  pub unsafe fn set_len(&mut self, new_len: usize) {
    debug_assert!(
      new_len <= N,
      "Attempted to unsafely set length to {}; maximum is {}!",
      new_len,
      N
    );
    self.length = new_len;
  }

  /// Returns true if the current length of the StaticVec is 0.
  #[inline(always)]
  pub const fn is_empty(&self) -> bool {
    self.length == 0
  }

  /// Returns true if the current length of the StaticVec is greater than 0.
  #[inline(always)]
  pub const fn is_not_empty(&self) -> bool {
    self.length > 0
  }

  /// Returns true if the current length of the StaticVec is equal to its capacity.
  #[inline(always)]
  pub const fn is_full(&self) -> bool {
    self.length == N
  }

  /// Returns true if the current length of the StaticVec is less than its capacity.
  #[inline(always)]
  pub const fn is_not_full(&self) -> bool {
    self.length < N
  }

  /// Returns a constant pointer to the first element of the StaticVec's internal array.
  #[inline(always)]
  pub fn as_ptr(&self) -> *const T {
    self.data.as_ptr() as *const T
  }

  /// Returns a mutable pointer to the first element of the StaticVec's internal array.
  #[inline(always)]
  pub fn as_mut_ptr(&mut self) -> *mut T {
    self.data.as_mut_ptr() as *mut T
  }

  /// Returns a constant reference to a slice of the StaticVec's inhabited area.
  #[inline(always)]
  pub fn as_slice(&self) -> &[T] {
    // Safety: `self.as_ptr()` is a pointer to an array for which the first `length`
    // elements are guaranteed to be initialized. Therefore this is a valid slice.
    unsafe { slice::from_raw_parts(self.as_ptr(), self.length) }
  }

  /// Returns a mutable reference to a slice of the StaticVec's inhabited area.
  #[inline(always)]
  pub fn as_mut_slice(&mut self) -> &mut [T] {
    // Safety: See as_slice.
    unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.length) }
  }

  /// Returns a constant reference to the element of the StaticVec at `index`,
  /// if `index` is within the range `0..length`. No checks are performed to
  /// ensure that is the case, so this function is marked `unsafe` and should
  /// be used with caution only when performance is absolutely paramount.
  ///
  /// Note that unlike [`slice::get_unchecked`](https://doc.rust-lang.org/nightly/std/primitive.slice.html#method.get_unchecked),
  /// this method only supports accessing individual elements via `usize`; it cannot also produce
  /// subslices. To unsafely get a subslice without a bounds check, use
  /// `self.as_slice().get_unchecked(a..b)`.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that `index` is within the appropriate bounds.
  #[inline(always)]
  pub unsafe fn get_unchecked(&self, index: usize) -> &T {
    debug_assert!(
      index < self.length,
      "Attempted to unsafely get at index {} when length is {}!",
      index,
      self.length
    );
    self.data.get_unchecked(index).get_ref()
  }

  /// Returns a mutable reference to the element of the StaticVec at `index`,
  /// if `index` is within the range `0..length`. No checks are performed to
  /// ensure that is the case, so this function is marked `unsafe` and should
  /// be used with caution only when performance is absolutely paramount.
  ///
  /// The same differences between this method and the slice method of the same name
  /// apply as do for [`get_unchecked`](crate::StaticVec::get_unchecked).
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that `index` is within the appropriate bounds.
  #[inline(always)]
  pub unsafe fn get_unchecked_mut(&mut self, index: usize) -> &mut T {
    debug_assert!(
      index < self.length,
      "Attempted to unsafely get at index {} when length is {}!",
      index,
      self.length
    );
    self.data.get_unchecked_mut(index).get_mut()
  }

  /// Returns a constant pointer to the element of the StaticVec at `index` without doing any
  /// checking to ensure that `index` is within the range `0..length`. The return value of this
  /// function is equivalent to what would be returned from `as_ptr().add(index)`.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that `index` is within the appropriate bounds such that the
  /// function returns a pointer to valid data.
  #[inline(always)]
  pub unsafe fn ptr_at_unchecked(&self, index: usize) -> *const T {
    self.as_ptr().add(index)
  }

  /// Returns a mutable pointer to the element of the StaticVec at `index` without doing any
  /// checking to ensure that `index` is within the range `0..length`. The return value of this
  /// function is equivalent to what would be returned from `as_mut_ptr().add(index)`.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that `index` is within the appropriate bounds such that the
  /// function returns a pointer to valid data.
  #[inline(always)]
  pub unsafe fn mut_ptr_at_unchecked(&mut self, index: usize) -> *mut T {
    self.as_mut_ptr().add(index)
  }

  /// Returns a constant pointer to the element of the StaticVec at `index` if `index`
  /// is within the range `0..length`, or panics if it is not. The return value of this function is
  /// equivalent to what would be returned from `as_ptr().add(index)`.
  #[inline(always)]
  pub fn ptr_at(&self, index: usize) -> *const T {
    assert!(
      index < self.length,
      "Provided index {} must be between 0 and {}!",
      index,
      self.length
    );
    unsafe { self.ptr_at_unchecked(index) }
  }

  /// Returns a mutable pointer to the element of the StaticVec at `index` if `index`
  /// is within the range `0..length`, or panics if it is not. The return value of this function is
  /// equivalent to what would be returned from `as_mut_ptr().add(index)`.
  #[inline(always)]
  pub fn mut_ptr_at(&mut self, index: usize) -> *mut T {
    assert!(
      index < self.length,
      "Provided index {} must be between 0 and {}!",
      index,
      self.length
    );
    unsafe { self.mut_ptr_at_unchecked(index) }
  }

  /// Appends a value to the end of the StaticVec without asserting that
  /// its current length is less than `N`.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that the length of the StaticVec
  /// prior to using this function is less than `N`. Failure to do so will result
  /// in writing to an out-of-bounds memory region.
  #[inline(always)]
  pub unsafe fn push_unchecked(&mut self, value: T) {
    debug_assert!(
      self.is_not_full(),
      "Attempted to unsafely push to a full StaticVec!"
    );
    self.mut_ptr_at_unchecked(self.length).write(value);
    self.length += 1;
  }

  /// Pops a value from the end of the StaticVec and returns it directly without asserting that
  /// the StaticVec's current length is greater than 0.
  ///
  /// # Safety
  ///
  /// It is up to the caller to ensure that the StaticVec contains at least one
  /// element prior to using this function. Failure to do so will result in reading
  /// from uninitialized memory.
  #[inline(always)]
  pub unsafe fn pop_unchecked(&mut self) -> T {
    debug_assert!(
      self.is_not_empty(),
      "Attempted to unsafely pop from an empty StaticVec!"
    );
    self.length -= 1;
    self.ptr_at_unchecked(self.length).read()
  }

  /// Pushes `value` to the StaticVec if its current length is less than its capacity,
  /// or returns an error indicating there's no remaining capacity otherwise.
  #[inline(always)]
  pub fn try_push(&mut self, value: T) -> Result<(), &'static str> {
    if self.length < N {
      unsafe {
        self.push_unchecked(value);
      }
      return Ok(());
    }
    Err("Insufficient remaining capacity!")
  }

  /// Pushes a value to the end of the StaticVec. Panics if the collection is
  /// full; that is, if `self.len() == self.capacity()`.
  #[inline(always)]
  pub fn push(&mut self, value: T) {
    assert!(self.length < N, "Insufficient remaining capacity!");
    unsafe { self.push_unchecked(value) }
  }

  /// Removes the value at the last position of the StaticVec and returns it in `Some` if
  /// the StaticVec has a current length greater than 0, and returns `None` otherwise.
  #[inline(always)]
  pub fn pop(&mut self) -> Option<T> {
    if self.is_empty() {
      None
    } else {
      Some(unsafe { self.pop_unchecked() })
    }
  }

  /// Returns a constant reference to the first element of the StaticVec in `Some` if the StaticVec
  /// is not empty, or `None` otherwise.
  #[inline(always)]
  pub fn first(&self) -> Option<&T> {
    if self.is_empty() {
      None
    } else {
      Some(unsafe { self.get_unchecked(0) })
    }
  }

  /// Returns a mutable reference to the first element of the StaticVec in `Some` if the StaticVec
  /// is not empty, or `None` otherwise.
  #[inline(always)]
  pub fn first_mut(&mut self) -> Option<&mut T> {
    if self.is_empty() {
      None
    } else {
      Some(unsafe { self.get_unchecked_mut(0) })
    }
  }

  /// Returns a constant reference to the last element of the StaticVec in `Some` if the StaticVec
  /// is not empty, or `None` otherwise.
  #[inline(always)]
  pub fn last(&self) -> Option<&T> {
    if self.is_empty() {
      None
    } else {
      Some(unsafe { self.get_unchecked(self.length - 1) })
    }
  }

  /// Returns a mutable reference to the last element of the StaticVec in `Some` if the StaticVec is
  /// not empty, or `None` otherwise.
  #[inline(always)]
  pub fn last_mut(&mut self) -> Option<&mut T> {
    if self.is_empty() {
      None
    } else {
      Some(unsafe { self.get_unchecked_mut(self.length - 1) })
    }
  }

  /// Asserts that `index` is less than the current length of the StaticVec,
  /// and if so removes the value at that position and returns it. Any values
  /// that exist in later positions are shifted to the left.
  #[inline]
  pub fn remove(&mut self, index: usize) -> T {
    assert!(index < self.length);
    unsafe {
      let p = self.mut_ptr_at_unchecked(index);
      let res = p.read();
      p.offset(1).copy_to(p, self.length - index - 1);
      self.length -= 1;
      res
    }
  }

  /// Removes the first instance of `item` from the StaticVec if the item exists.
  #[inline(always)]
  pub fn remove_item(&mut self, item: &T) -> Option<T>
  where T: PartialEq {
    // Adapted this from normal Vec's implementation.
    if let Some(pos) = self.iter().position(|x| *x == *item) {
      Some(self.remove(pos))
    } else {
      None
    }
  }

  /// Returns `None` if `index` is greater than or equal to the current length of the StaticVec.
  /// Otherwise, removes the value at that position and returns it in `Some`, and then
  /// moves the last value in the StaticVec into the empty slot.
  #[inline(always)]
  pub fn swap_pop(&mut self, index: usize) -> Option<T> {
    if index < self.length {
      unsafe {
        let last_value = self.data.get_unchecked(self.length - 1).read();
        self.length -= 1;
        Some(self.mut_ptr_at_unchecked(index).replace(last_value))
      }
    } else {
      None
    }
  }

  /// Asserts that `index` is less than the current length of the StaticVec,
  /// and if so removes the value at that position and returns it, and then
  /// moves the last value in the StaticVec into the empty slot.
  #[inline(always)]
  pub fn swap_remove(&mut self, index: usize) -> T {
    assert!(index < self.length);
    unsafe {
      let last_value = self.data.get_unchecked(self.length - 1).read();
      self.length -= 1;
      self.mut_ptr_at_unchecked(index).replace(last_value)
    }
  }

  /// Asserts that the current length of the StaticVec is less than `N` and that
  /// `index` is less than the length, and if so inserts `value` at that position.
  /// Any values that exist in positions after `index` are shifted to the right.
  #[inline]
  pub fn insert(&mut self, index: usize, value: T) {
    assert!(self.length < N && index <= self.length);
    unsafe {
      let p = self.mut_ptr_at_unchecked(index);
      p.copy_to(p.offset(1), self.length - index);
      p.write(value);
      self.length += 1;
    }
  }

  /// Inserts `value` at `index` if the current length of the StaticVec is less than `N` and `index`
  /// is less than the length, or returns a error stating one of the two is not the case otherwise.
  /// Any values that exist in positions after `index` are shifted to the right.
  #[inline]
  pub fn try_insert(&mut self, index: usize, value: T) -> Result<(), &'static str> {
    if self.length < N && index <= self.length {
      unsafe {
        let p = self.mut_ptr_at_unchecked(index);
        p.copy_to(p.offset(1), self.length - index);
        p.write(value);
        self.length += 1;
        Ok(())
      }
    } else {
      Err("One of `self.length < N` or `index <= self.length` is false!")
    }
  }

  /// Removes all contents from the StaticVec and sets its length back to 0.
  #[inline(always)]
  pub fn clear(&mut self) {
    unsafe {
      ptr::drop_in_place(self.as_mut_slice());
    }
    self.length = 0;
  }

  /// Returns a [`StaticVecIterConst`](crate::iterators::StaticVecIterConst) over the StaticVec's
  /// inhabited area.
  #[inline(always)]
  pub fn iter<'a>(&'a self) -> StaticVecIterConst<'a, T, N> {
    StaticVecIterConst {
      start: self.as_ptr(),
      end: match intrinsics::size_of::<T>() {
        0 => (self.as_ptr() as *const u8).wrapping_add(self.length) as *const T,
        _ => unsafe { self.ptr_at_unchecked(self.length) },
      },
      marker: PhantomData,
    }
  }

  /// Returns a [`StaticVecIterMut`](crate::iterators::StaticVecIterMut) over the StaticVec's
  /// inhabited area.
  #[inline(always)]
  pub fn iter_mut<'a>(&'a mut self) -> StaticVecIterMut<'a, T, N> {
    StaticVecIterMut {
      start: self.as_mut_ptr(),
      end: match intrinsics::size_of::<T>() {
        0 => (self.as_mut_ptr() as *mut u8).wrapping_add(self.length) as *mut T,
        _ => unsafe { self.mut_ptr_at_unchecked(self.length) },
      },
      marker: PhantomData,
    }
  }

  /// Returns a separate, stable-sorted StaticVec of the contents of the
  /// StaticVec's inhabited area without modifying the original data.
  /// Locally requires that `T` implements [`Copy`](core::marker::Copy) to avoid soundness issues,
  /// and [`Ord`](core::cmp::Ord) to make the sorting possible.
  #[cfg(feature = "std")]
  #[doc(cfg(feature = "std"))]
  #[inline]
  pub fn sorted(&self) -> Self
  where T: Copy + Ord {
    let mut res = self.clone();
    res.sort();
    res
  }

  /// Returns a separate, unstable-sorted StaticVec of the contents of the
  /// StaticVec's inhabited area without modifying the original data.
  /// Locally requires that `T` implements [`Copy`](core::marker::Copy) to avoid soundness issues,
  /// and [`Ord`](core::cmp::Ord) to make the sorting possible.
  #[inline]
  pub fn sorted_unstable(&self) -> Self
  where T: Copy + Ord {
    let mut res = self.clone();
    res.sort_unstable();
    res
  }

  /// Returns a separate, reversed StaticVec of the contents of the StaticVec's
  /// inhabited area without modifying the original data.
  /// Locally requires that `T` implements [`Copy`](core::marker::Copy) to avoid soundness issues.
  #[inline]
  pub fn reversed(&self) -> Self
  where T: Copy {
    Self {
      data: unsafe {
        let mut res = Self::new_data_uninit();
        reverse_copy(
          self.as_ptr(),
          self.ptr_at_unchecked(self.length),
          res.as_mut_ptr() as *mut T,
        );
        res.assume_init()
      },
      length: self.length,
    }
  }

  /// Copies and appends all elements, if any, of a slice (which can also be `&mut` as it will
  /// coerce implicitly to `&`) to the StaticVec. If the slice has a length greater than the
  /// StaticVec's remaining capacity, any contents after that point are ignored.
  /// Locally requires that `T` implements [`Copy`](core::marker::Copy) to avoid soundness issues.
  #[inline(always)]
  pub fn extend_from_slice(&mut self, other: &[T])
  where T: Copy {
    let added_length = other.len().min(self.remaining_capacity());
    // Safety: added_length is <= our remaining capacity and other.len.
    unsafe {
      other
        .as_ptr()
        .copy_to_nonoverlapping(self.mut_ptr_at_unchecked(self.length), added_length);
    }
    self.length += added_length;
  }

  /// Copies and appends all elements, if any, of a slice to the StaticVec if the
  /// StaticVec's remaining capacity is greater than the length of the slice, or returns
  /// an error indicating that's not the case otherwise.
  #[inline(always)]
  pub fn try_extend_from_slice(&mut self, other: &[T]) -> Result<(), &'static str>
  where T: Copy {
    let added_length = other.len();
    if self.remaining_capacity() < added_length {
      return Err("Insufficient remaining capacity!");
    }
    unsafe {
      other
        .as_ptr()
        .copy_to_nonoverlapping(self.mut_ptr_at_unchecked(self.length), added_length);
    }
    self.length += added_length;
    Ok(())
  }

  /// Appends `self.remaining_capacity()` (or as many as available) items from
  /// `other` to `self`. The appended items (if any) will no longer exist in `other` afterwards,
  /// as `other`'s `length` field will be adjusted to indicate.
  ///
  /// The `N2` parameter does not need to be provided explicitly, and can be inferred directly from
  /// the constant `N2` constraint of `other` (which may or may not be the same as the `N`
  /// constraint of `self`.)
  #[inline]
  pub fn append<const N2: usize>(&mut self, other: &mut StaticVec<T, N2>) {
    let item_count = self.remaining_capacity().min(other.length);
    let other_new_length = other.length - item_count;
    unsafe {
      self
        .mut_ptr_at_unchecked(self.length)
        .copy_from_nonoverlapping(other.as_ptr(), item_count);
      other
        .as_mut_ptr()
        .copy_from(other.ptr_at_unchecked(item_count), other_new_length);
    }
    other.length = other_new_length;
    self.length += item_count;
  }

  /// Returns a [`Vec`](alloc::vec::Vec) containing the contents of the StaticVec instance.
  /// The returned [`Vec`](alloc::vec::Vec) will initially have the same value for
  /// [`len`](alloc::vec::Vec::len) and [`capacity`](alloc::vec::Vec::capacity) as the source
  /// StaticVec. Note that while using this function does *not* consume the source StaticVec in
  /// the sense of rendering it completely inaccessible / unusable, it *does* empty it (that is,
  /// it will have no contents and a length of 0 afterwards.)
  #[cfg(feature = "std")]
  #[doc(cfg(feature = "std"))]
  #[inline(always)]
  pub fn into_vec(&mut self) -> Vec<T> {
    let mut res = Vec::with_capacity(N);
    unsafe {
      self
        .as_ptr()
        .copy_to_nonoverlapping(res.as_mut_ptr(), self.length);
      res.set_len(self.length);
      self.length = 0;
      res
    }
  }

  /// Removes the specified range of elements from the StaticVec and returns them in a new one.
  #[inline]
  pub fn drain<R>(&mut self, range: R) -> Self
  // No Copy bounds here because the original StaticVec gives up all access to the values in
  // question.
  where R: RangeBounds<usize> {
    // Borrowed this part from normal Vec's implementation.
    let start = match range.start_bound() {
      Included(&idx) => idx,
      Excluded(&idx) => idx + 1,
      Unbounded => 0,
    };
    let end = match range.end_bound() {
      Included(&idx) => idx + 1,
      Excluded(&idx) => idx,
      Unbounded => self.length,
    };
    assert!(start <= end && end <= self.length);
    let res_length = end - start;
    Self {
      data: {
        let mut res = Self::new_data_uninit();
        unsafe {
          self
            .ptr_at_unchecked(start)
            .copy_to_nonoverlapping(res.as_mut_ptr() as *mut T, res_length);
          self
            .ptr_at_unchecked(end)
            .copy_to(self.mut_ptr_at_unchecked(start), self.length - end);
          self.length -= res_length;
          res.assume_init()
        }
      },
      length: res_length,
    }
  }

  /// Removes all elements in the StaticVec for which `filter` returns true and
  /// returns them in a new one.
  #[inline]
  pub fn drain_filter<F>(&mut self, mut filter: F) -> Self
  where F: FnMut(&mut T) -> bool {
    let mut res = Self::new();
    let old_length = self.length;
    self.length = 0;
    unsafe {
      for i in 0..old_length {
        let val = self.mut_ptr_at_unchecked(i);
        if filter(&mut *val) {
          res.data.get_unchecked_mut(res.length).write(val.read());
          res.length += 1;
        } else if res.length > 0 {
          self
            .ptr_at_unchecked(i)
            .copy_to_nonoverlapping(self.mut_ptr_at_unchecked(i - res.length), 1);
        }
      }
    }
    self.length = old_length - res.length;
    res
  }

  /// Removes all elements in the StaticVec for which `filter` returns false.
  #[inline(always)]
  pub fn retain<F>(&mut self, mut filter: F)
  where F: FnMut(&T) -> bool {
    self.drain_filter(|val| !filter(val));
  }

  /// Shortens the StaticVec, keeping the first `length` elements and dropping the rest.
  /// Does nothing if `length` is greater than or equal to the current length of the StaticVec.
  #[inline(always)]
  pub fn truncate(&mut self, length: usize) {
    if length < self.length {
      let old_length = self.length;
      self.length = length;
      unsafe {
        ptr::drop_in_place(self.as_mut_slice().get_unchecked_mut(length..old_length));
      }
    }
  }

  /// Splits the StaticVec into two at the given index.
  /// The original StaticVec will contain elements `0..at`,
  /// and the new one will contain elements `at..length`.
  #[inline]
  pub fn split_off(&mut self, at: usize) -> Self {
    assert!(at <= self.length);
    let split_length = self.length - at;
    self.length = at;
    Self {
      data: unsafe {
        let mut split = Self::new_data_uninit();
        self
          .ptr_at_unchecked(at)
          .copy_to_nonoverlapping(split.as_mut_ptr() as *mut T, split_length);
        split.assume_init()
      },
      length: split_length,
    }
  }

  /// Removes all but the first of consecutive elements in the StaticVec satisfying a given equality
  /// relation.
  #[inline(always)]
  pub fn dedup_by<F>(&mut self, same_bucket: F)
  where F: FnMut(&mut T, &mut T) -> bool {
    // Mostly the same as Vec's version.
    let new_length = self.as_mut_slice().partition_dedup_by(same_bucket).0.len();
    self.truncate(new_length);
  }

  /// Removes consecutive repeated elements in the StaticVec according to the
  /// locally required [`PartialEq`](core::cmp::PartialEq) trait implementation for `T`.
  #[inline(always)]
  pub fn dedup(&mut self)
  where T: PartialEq {
    // Exactly the same as Vec's version.
    self.dedup_by(|a, b| a == b)
  }

  /// Removes all but the first of consecutive elements in the StaticVec that
  /// resolve to the same key.
  #[inline(always)]
  pub fn dedup_by_key<F, K>(&mut self, mut key: F)
  where
    F: FnMut(&mut T) -> K,
    K: PartialEq<K>, {
    // Exactly the same as Vec's version.
    self.dedup_by(|a, b| key(a) == key(b))
  }

  #[doc(hidden)]
  #[inline(always)]
  pub(crate) fn new_data() -> [MaybeUninit<T>; N] {
    // An internal convenience function to get an *initialized* instance of `[MaybeUninit<T>; N]`.
    MaybeUninit::uninit_array()
  }

  #[doc(hidden)]
  #[inline(always)]
  pub(crate) const fn new_data_uninit() -> MaybeUninit<[MaybeUninit<T>; N]> {
    // An internal convenience function to get an *uninitialized* instance of
    // `MaybeUninit<[MaybeUninit<T>; N]>`.
    MaybeUninit::uninit()
  }
}
