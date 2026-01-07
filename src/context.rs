//! Deobfuscation context and shared state.
//!
//! `Context` is the internal state passed through the transformer pipeline.
//! It stores the AST and the metadata collected by earlier passes (string
//! arrays, decoder functions, control-flow storage, etc.).

use std::collections::HashMap;
use std::fmt;

use swc_ecma_ast::{Function, Lit, Program};

use crate::transformers::TransformerBox;

/// Decoder function types for string array decoding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DecoderFunctionType {
    /// Direct string array lookup without extra decoding.
    Simple,
    /// Base64 decoding with a custom charset.
    Base64,
    /// RC4 decryption with a custom charset and key.
    Rc4,
    /// Base91 decoding with a custom charset.
    Base91,
}

/// Information about a string decoder function
#[derive(Debug, Clone)]
pub struct DecoderFunction {
    /// Decoder function identifier.
    pub identifier: String,
    /// Referenced string array identifier.
    pub string_array_identifier: String,
    /// Decoder function category.
    pub decoder_type: DecoderFunctionType,
    /// Offset applied to the index argument.
    pub offset: i32,
    /// Index argument position at call sites.
    pub index_argument: usize,
    /// Key argument position for keyed decoders.
    pub key_argument: usize,
    /// For Base64/RC4: the charset used
    pub charset: Option<String>,
}

/// Reference to a decoder function (wrapper/alias)
#[derive(Debug, Clone)]
pub struct DecoderReference {
    /// Wrapper identifier.
    pub identifier: String,
    /// Final decoder identifier.
    pub real_identifier: String,
    /// Offset added by the wrapper.
    pub additional_offset: i32,
    /// If the wrapper is a function
    pub index_argument: Option<usize>,
    /// Key argument position for wrappers that forward a key.
    pub key_argument: Option<usize>,
}

/// Type of string array storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StringArrayType {
    /// String array stored in a function that returns the array.
    Function,
    /// String array stored as a variable array literal.
    Array,
}

/// String array information
#[derive(Debug, Clone)]
pub struct StringArray {
    /// Array identifier.
    pub identifier: String,
    /// Storage form for the string array.
    pub array_type: StringArrayType,
    /// Collected string literals.
    pub strings: Vec<String>,
}

/// Control flow storage for a block
#[derive(Debug, Clone)]
pub struct ControlFlowStorage {
    /// Storage identifier.
    pub identifier: String,
    /// Aliases referencing the storage.
    pub aliases: Vec<String>,
    /// Stored control-flow functions.
    pub functions: Vec<ControlFlowFunction>,
    /// Stored literal values.
    pub literals: Vec<ControlFlowLiteral>,
}

/// Function stored in control flow storage
#[derive(Debug, Clone)]
pub struct ControlFlowFunction {
    /// Function identifier.
    pub identifier: String,
    /// Function AST node.
    pub node: Box<Function>,
}

/// Literal stored in control flow storage
#[derive(Debug, Clone)]
pub struct ControlFlowLiteral {
    /// Literal identifier.
    pub identifier: String,
    /// Literal value.
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
    pub control_flow_storage_nodes: HashMap<String, ControlFlowStorage>,

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
            control_flow_storage_nodes: HashMap::new(),
            remove_garbage: true,
            rename_enabled: false,
            transformers,
        }
    }
}
