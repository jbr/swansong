use super::Interrupt;
impl<T: Iterator> Iterator for Interrupt<T> {
    type Item = T::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_stopped() {
            None
        } else {
            self.wrapped_type.next()
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.wrapped_type.size_hint().1)
    }
}
