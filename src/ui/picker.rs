/// A bounded cursor over a list of rows. Which pane is open is what gives
/// the rows their meaning, so the cursor itself carries none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    index: usize,
    len: usize,
}

impl Picker {
    pub fn open(len: usize, at: usize) -> Picker {
        Picker {
            index: at.min(len.saturating_sub(1)),
            len,
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.len == 0 {
            return;
        }
        let max = (self.len - 1) as isize;
        self.index = (self.index as isize + delta).clamp(0, max) as usize;
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_clamps_at_both_ends() {
        let mut p = Picker::open(3, 0);
        p.move_by(-1);
        assert_eq!(p.index(), 0);
        p.move_by(10);
        assert_eq!(p.index(), 2);
    }

    #[test]
    fn opening_past_the_end_lands_on_the_last_row() {
        assert_eq!(Picker::open(3, 99).index(), 2);
    }

    #[test]
    fn an_empty_picker_has_no_cursor_to_move() {
        let mut p = Picker::open(0, 0);
        p.move_by(1);
        assert_eq!(p.index(), 0);
        assert!(p.is_empty());
    }
}
