use crate::typed::snapshot::FlatEntityRef;

#[derive(derive_more::Debug, Clone)]
pub struct Blacklist<F> {
    #[debug("<function>")]
    list: F,
}

impl<F> Blacklist<F>
where
    F: Fn(FlatEntityRef) -> bool + Clone,
{
    pub fn new(list: F) -> Self {
        Self { list }
    }
}
pub trait IsSet: Clone {
    fn contains(&self, entity: FlatEntityRef) -> bool;
}
impl<F> IsSet for Blacklist<F>
where
    F: Fn(FlatEntityRef) -> bool + Clone,
{
    fn contains(&self, entity: FlatEntityRef) -> bool {
        (self.list)(entity)
    }
}
