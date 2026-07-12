mod compiler;
mod parser;
mod resolver;

pub use compiler::CompiledTemplate;
pub use parser::{ParsedReference, is_template};
pub use resolver::TemplateResolver;
