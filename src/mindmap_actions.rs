#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MindmapToggleTodo;

impl gpui_kit::Action for MindmapToggleTodo {
    fn boxed_clone(&self) -> Box<dyn gpui_kit::Action> {
        Box::new(*self)
    }

    fn partial_eq(&self, action: &dyn gpui_kit::Action) -> bool {
        action
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn name(&self) -> &'static str {
        "kite::MindmapToggleTodo"
    }

    fn name_for_type() -> &'static str
    where
        Self: Sized,
    {
        "kite::MindmapToggleTodo"
    }

    fn build(
        _value: gpui_kit::private::serde_json::Value,
    ) -> gpui_kit::Result<Box<dyn gpui_kit::Action>> {
        Ok(Box::new(Self))
    }
}
