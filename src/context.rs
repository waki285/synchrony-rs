//! Deobfuscation context and shared state.
//!
//! `Context` is the internal state passed through the transformer pipeline.
//! It stores the AST and the metadata collected by earlier passes (string
//! arrays, decoder functions, control-flow storage, etc.).

use std::fmt;

use swc_ecma_ast::{Function, Lit, Program};

use crate::transformers::TransformerBox;

/// Decoder function types for string array decoding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecoderFunctionType {
    Simple,
    Base64,
    Rc4,
    Base91,
}

/// Information about a string decoder function
#[derive(Debug, Clone)]
pub struct DecoderFunction {
    pub identifier: String,
    pub string_array_identifier: String,
    pub decoder_type: DecoderFunctionType,
    pub offset: i32,
    pub index_argument: usize,
    pub key_argument: usize,
    /// For Base64/RC4: the charset used
    pub charset: Option<String>,
}

/// Reference to a decoder function (wrapper/alias)
#[derive(Debug, Clone)]
pub struct DecoderReference {
    pub identifier: String,
    pub real_identifier: String,
    pub additional_offset: i32,
    /// If the wrapper is a function
    pub index_argument: Option<usize>,
    pub key_argument: Option<usize>,
}

/// Type of string array storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringArrayType {
    Function,
    Array,
}

/// String array information
#[derive(Debug, Clone)]
pub struct StringArray {
    pub identifier: String,
    pub array_type: StringArrayType,
    pub strings: Vec<String>,
}

/// Control flow storage for a block
#[derive(Debug, Clone)]
pub struct ControlFlowStorage {
    pub identifier: String,
    pub aliases: Vec<String>,
    pub functions: Vec<ControlFlowFunction>,
    pub literals: Vec<ControlFlowLiteral>,
}

/// Function stored in control flow storage
#[derive(Debug, Clone)]
pub struct ControlFlowFunction {
    pub identifier: String,
    pub node: Box<Function>,
}

/// Literal stored in control flow storage
#[derive(Debug, Clone)]
pub struct ControlFlowLiteral {
    pub identifier: String,
    pub value: Lit,
}

/// Context for the deobfuscation process
///
/// This holds the AST and all state that is shared between transformers.
#[derive(Clone)]
pub struct Context {
    /// The AST being transformed
    pub ast: Program,

    /// Source code (if available)
    pub source: Option<String>,

    /// Whether the source is an ES module
    pub is_module: bool,

    /// Hash of the source (used for renaming)
    pub hash: u32,

    /// Number of arrays that have been shifted
    pub shifted_arrays: usize,

    /// Detected string arrays
    pub string_arrays: Vec<StringArray>,

    /// Detected string decoder functions
    pub string_decoders: Vec<DecoderFunction>,

    /// References to string decoders
    pub string_decoder_references: Vec<DecoderReference>,

    /// Control flow storage nodes by block ID
    pub control_flow_storage_nodes: std::collections::HashMap<String, ControlFlowStorage>,

    /// Whether to remove garbage/dead code
    pub remove_garbage: bool,

    /// Whether a rename pass will be applied after the main pipeline
    pub rename_enabled: bool,

    /// List of transformers to run
    pub transformers: Vec<TransformerBox>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("ast", &self.ast)
            .field("source", &self.source)
            .field("is_module", &self.is_module)
            .field("hash", &self.hash)
            .field("shifted_arrays", &self.shifted_arrays)
            .field("string_arrays", &self.string_arrays)
            .field("string_decoders", &self.string_decoders)
            .field("string_decoder_references", &self.string_decoder_references)
            .field(
                "control_flow_storage_nodes",
                &self.control_flow_storage_nodes,
            )
            .field("remove_garbage", &self.remove_garbage)
            .field("rename_enabled", &self.rename_enabled)
            .field("transformers_len", &self.transformers.len())
            .finish()
    }
}

impl Context {
    /// Create a new context with the given AST and transformers
    #[must_use]
    pub fn new(
        ast: Program,
        transformers: Vec<TransformerBox>,
        is_module: bool,
        source: Option<String>,
    ) -> Self {
        Self {
            ast,
            source,
            is_module,
            hash: 0,
            shifted_arrays: 0,
            string_arrays: Vec::new(),
            string_decoders: Vec::new(),
            string_decoder_references: Vec::new(),
            control_flow_storage_nodes: std::collections::HashMap::new(),
            remove_garbage: true,
            rename_enabled: false,
            transformers,
        }
    }
}
