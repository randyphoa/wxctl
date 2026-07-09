/// Generate a `get_handler()` function that dispatches resource names to handler instances.
///
/// Usage:
/// ```ignore
/// define_handlers! {
///     "catalog" => handlers::CatalogHandler,
///     "space" => handlers::SpaceHandler,
/// }
/// ```
macro_rules! define_handlers {
    ( $( $name:literal => $handler:expr ),+ $(,)? ) => {
        pub fn get_handler(resource_name: &str) -> Option<::std::sync::Arc<dyn ::wxctl_core::traits::ResourceHandler>> {
            match resource_name {
                $( $name => Some(::std::sync::Arc::new($handler)), )+
                _ => None,
            }
        }
    };
}
