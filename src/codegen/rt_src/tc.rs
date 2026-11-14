#[allow(dead_code)]
pub struct Tc<S, R>(Option<Box<dyn FnOnce(&mut S, &mut Tc<S, R>) -> R>>);
#[allow(dead_code)]
impl<S, R> Tc<S, R> {
    fn new() -> Self {
        Tc(None)
    }
    fn set(&mut self, f: impl FnOnce(&mut S, &mut Tc<S, R>) -> R + 'static) {
        self.0 = Some(Box::new(f));
    }
    fn run(&mut self, s: &mut S, mut r: R) -> R {
        while let Some(f) = self.0.take() {
            r = f(s, self);
        }
        r
    }
}
