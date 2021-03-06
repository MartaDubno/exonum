// Copyright 2018 The Exonum Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `Transaction` related types.
use serde::{de::DeserializeOwned, Serialize};
use std::{any::Any, borrow::Cow, convert::Into, error::Error, fmt, u8};

use crypto::{CryptoHash, Hash, PublicKey};
use encoding;
use hex::ToHex;
use messages::{HexStringRepresentation, RawTransaction, Signed, SignedMessage};
use storage::{Fork, StorageValue};

//  User-defined error codes (`TransactionErrorType::Code(u8)`) have a `0...255` range.
#[cfg_attr(feature = "cargo-clippy", allow(cast_lossless))]
const MAX_ERROR_CODE: u16 = u8::max_value() as u16;
// Represent `(Ok())` `TransactionResult` value.
const TRANSACTION_STATUS_OK: u16 = MAX_ERROR_CODE + 1;
// `Err(TransactionErrorType::Panic)`.
const TRANSACTION_STATUS_PANIC: u16 = TRANSACTION_STATUS_OK + 1;

/// Returns a result of the `Transaction` `execute` method. This result may be
/// either an empty unit type, in case of success, or an `ExecutionError`, if execution has
/// failed. Errors consist of an error code and an optional description.
pub type ExecutionResult = Result<(), ExecutionError>;
/// Extended version of `ExecutionResult` (with additional values set exclusively by Exonum
/// framework) that can be obtained through `Schema` `transaction_statuses` method.
#[derive(Clone, Debug, PartialEq)]
pub struct TransactionResult(pub Result<(), TransactionError>);

/// Data transfer object for transaction.
/// This structure is used to send api info about transaction,
/// and take some new transaction into pool from user input.
#[derive(Serialize, Deserialize)]
pub struct TransactionMessage {
    #[serde(skip_deserializing)]
    #[serde(rename = "debug")]
    transaction: Option<Box<dyn Transaction>>,

    #[serde(with = "HexStringRepresentation")]
    message: Signed<RawTransaction>,
}
impl ::std::fmt::Debug for TransactionMessage {
    fn fmt(&self, fmt: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        let mut signed_message_debug = String::new();
        self.message
            .signed_message()
            .write_hex(&mut signed_message_debug)?;

        let mut debug = fmt.debug_struct("TransactionMessage");
        debug.field("message", &signed_message_debug);
        if let Some(ref tx) = self.transaction {
            debug.field("debug", tx);
        }
        debug.finish()
    }
}

impl TransactionMessage {
    /// Returns `SignedMessage`.
    pub fn signed_message(&self) -> &SignedMessage {
        self.message.signed_message()
    }
    /// Returns `RawTransaction`.
    pub fn raw_transaction(&self) -> RawTransaction {
        self.message.payload().clone()
    }
    /// Returns raw transaction message.
    pub fn message(&self) -> &Signed<RawTransaction> {
        &self.message
    }
    /// Returns transaction smart contract.
    pub fn transaction(&self) -> Option<&dyn Transaction> {
        use std::ops::Deref;
        self.transaction.as_ref().map(Deref::deref)
    }
    /// Create new `TransactionMessage` from raw message.
    pub(crate) fn new(
        message: Signed<RawTransaction>,
        transaction: Box<dyn Transaction>,
    ) -> TransactionMessage {
        TransactionMessage {
            transaction: Some(transaction),
            message,
        }
    }
}

impl ::serde::Serialize for dyn Transaction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ::serde::Serializer,
    {
        ::erased_serde::serialize(self, serializer)
    }
}

/// Transaction processing functionality for `Signed`s allowing to apply authenticated, atomic,
/// constraint-preserving groups of changes to the blockchain storage.
///
/// A transaction in Exonum is a group of sequential operations with the data.
/// Transaction processing rules are defined in services; these rules determine
/// the business logic of any Exonum-powered blockchain.
///
/// See also [the documentation page on transactions][doc:transactions].
///
/// [doc:transactions]: https://exonum.com/doc/architecture/transactions/
pub trait Transaction: ::std::fmt::Debug + Send + 'static + ::erased_serde::Serialize {
    /// Verifies the internal consistency of the transaction. `verify` should include
    /// only invariant checking. The message signature is checked internally.
    /// `verify` has no access to the blockchain state;
    /// checks involving the blockchain state must be preformed in [`execute`](#tymethod.execute).
    ///
    /// If a transaction fails `verify`, it is considered incorrect and cannot be included into
    /// any correct block proposal. Incorrect transactions are never included into the blockchain.
    ///
    /// *This method should not use external data, that is, it must be a pure function.*
    ///
    /// # Examples
    ///
    /// ```
    /// # #[macro_use] extern crate exonum;
    /// # #[macro_use] extern crate serde_derive;
    /// #
    /// use exonum::blockchain::{Transaction, TransactionContext};
    /// use exonum::crypto::PublicKey;
    /// use exonum::messages::Signed;
    /// # use exonum::blockchain::ExecutionResult;
    ///
    /// transactions! {
    ///     MyTransactions {
    ///
    ///         struct MyTransaction {
    ///             // Transaction definition...
    ///             public_key: &PublicKey,
    ///         }
    ///     }
    /// }
    ///
    /// impl Transaction for MyTransaction {
    ///     // Other methods...
    ///     // ...
    /// #   fn execute(&self, _: TransactionContext) -> ExecutionResult { Ok(()) }
    /// }
    /// # fn main() {}
    fn verify(&self) -> bool {
        true
    }

    /// Receives a `TransactionContext` witch contain fork
    /// of the current blockchain state and can modify it depending on the contents
    /// of the transaction.
    ///
    /// # Notes
    ///
    /// - Transaction itself is considered committed regardless whether `Ok` or `Err` has been
    ///   returned or even if panic occurs during execution.
    /// - Changes made by the transaction are discarded if `Err` is returned or panic occurs.
    /// - A transaction execution status (see `ExecutionResult` and `TransactionResult` for the
    ///   details) is stored in the blockchain and can be accessed through API.
    /// - Blockchain state hash is affected by the transactions execution status.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[macro_use] extern crate exonum;
    /// # #[macro_use] extern crate serde_derive;
    /// #
    /// use exonum::blockchain::{Transaction, ExecutionResult, TransactionContext};
    /// use exonum::crypto::PublicKey;
    /// use exonum::storage::Fork;
    ///
    /// transactions! {
    ///     MyTransactions {
    ///
    ///         struct MyTransaction {
    ///             // Transaction definition...
    ///             public_key: &PublicKey,
    ///         }
    ///     }
    /// }
    ///
    /// impl Transaction for MyTransaction {
    ///     fn execute(&self, _: TransactionContext) -> ExecutionResult {
    ///         // Read and/or write into storage.
    ///         // ...
    ///
    ///         // Return execution status.
    ///         Ok(())
    ///     }
    ///
    ///     // Other methods...
    ///     // ...
    /// }
    /// # fn main() {}
    fn execute<'a>(&self, context: TransactionContext<'a>) -> ExecutionResult;
}

//TODO: Add doc/examples.
/// Wrapper around database and tx hash.
#[derive(Debug)]
pub struct TransactionContext<'a> {
    fork: &'a mut Fork,
    service_id: u16,
    tx_hash: Hash,
    author: PublicKey,
}

impl<'a> TransactionContext<'a> {
    pub(crate) fn new(fork: &'a mut Fork, raw_message: &Signed<RawTransaction>) -> Self {
        TransactionContext {
            fork,
            service_id: raw_message.service_id(),
            tx_hash: raw_message.hash(),
            author: raw_message.author(),
        }
    }
    /// Returns fork of current blockchain state.
    pub fn fork(&mut self) -> &mut Fork {
        self.fork
    }
    /// Returns id of service that own this transaction.
    pub fn service_id(&self) -> u16 {
        self.service_id
    }
    /// Returns transaction author public key
    pub fn author(&self) -> PublicKey {
        self.author
    }
    /// Returns current transaction message hash.
    /// This hash could be used to link some data in storage for external usage.
    pub fn tx_hash(&self) -> Hash {
        self.tx_hash
    }
}

/// Result of unsuccessful transaction execution.
///
/// An execution error consists
/// of an error code and optional description. The error code affects the blockchain
/// state hash, while the description does not. Therefore,
/// descriptions are mostly used for developer purposes, not for interaction of
/// the system with users.
///
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutionError {
    /// User-defined error code. Error codes can have different meanings for different
    /// transactions and services.
    code: u8,
    /// Optional error description.
    description: Option<String>,
}

impl ExecutionError {
    /// Constructs a new `ExecutionError` instance with the given error code.
    pub fn new(code: u8) -> Self {
        Self {
            code,
            description: None,
        }
    }

    /// Constructs a new `ExecutionError` instance with the given error code and description.
    pub fn with_description<T: Into<String>>(code: u8, description: T) -> Self {
        Self {
            code,
            description: Some(description.into()),
        }
    }
}

/// Type of transaction error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransactionErrorType {
    /// Panic occurred during transaction execution.
    Panic,
    /// User-defined error code. Can have different meanings for different transactions and
    /// services.
    Code(u8),
}

/// Result of unsuccessful transaction execution encompassing both service and framework-wide error
/// handling.
/// This error indicates whether a panic or a user error has occurred.
///
/// # Notes:
///
/// - Content of the `description` field is excluded from the hash calculation (see `StorageValue`
///   implementation for the details).
/// - `TransactionErrorType::Panic` is set by the framework if panic is raised during transaction
///   execution.
/// - `TransactionError` implements `Display` which can be used for obtaining a simple error
///   description.
///
/// # Examples
///
/// The example below creates a schema; retrieves the table
/// with transaction results from this schema; using a hash takes the result
/// of a certain transaction and returns a message that depends on whether the
/// transaction is successful or not.
///
/// ```
/// # use exonum::storage::{MemoryDB, Database};
/// # use exonum::crypto::Hash;
/// use exonum::blockchain::Schema;
///
/// # let db = MemoryDB::new();
/// # let snapshot = db.snapshot();
/// # let transaction_hash = Hash::zero();
/// let schema = Schema::new(&snapshot);
///
/// if let Some(result) = schema.transaction_results().get(&transaction_hash) {
///     match result.0 {
///         Ok(()) => println!("Successful transaction execution"),
///         Err(transaction_error) => {
///             // Prints user friendly error description.
///             println!("Transaction error: {}", transaction_error);
///         }
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransactionError {
    /// Error type, see `TransactionErrorType` for the details.
    error_type: TransactionErrorType,
    /// Optional error description.
    description: Option<String>,
}

impl TransactionError {
    /// Creates a new `TransactionError` instance with the specified error type and description.
    fn new(error_type: TransactionErrorType, description: Option<String>) -> Self {
        Self {
            error_type,
            description,
        }
    }

    /// Creates a new `TransactionError` instance with the specified error code and description.
    pub(crate) fn code(code: u8, description: Option<String>) -> Self {
        Self::new(TransactionErrorType::Code(code), description)
    }

    /// Creates a new `TransactionError` representing panic with the given description.
    pub(crate) fn panic(description: Option<String>) -> Self {
        Self::new(TransactionErrorType::Panic, description)
    }

    /// Creates a new `TransactionError` instance from `std::thread::Result`'s `Err`.
    pub(crate) fn from_panic(panic: &Box<dyn Any + Send>) -> Self {
        Self::panic(panic_description(panic))
    }

    /// Returns an error type of this `TransactionError` instance. This can be
    /// either a panic or a user-defined error code.
    pub fn error_type(&self) -> TransactionErrorType {
        self.error_type
    }

    /// Returns an optional error description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_ref().map(String::as_ref)
    }
}

impl<'a, T: Transaction> From<T> for Box<dyn Transaction + 'a> {
    fn from(tx: T) -> Self {
        Box::new(tx) as Self
    }
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.error_type {
            TransactionErrorType::Panic => write!(f, "Panic during execution")?,
            TransactionErrorType::Code(c) => write!(f, "Error code: {}", c)?,
        }

        if let Some(ref description) = self.description {
            write!(f, " description: {}", description)?;
        }

        Ok(())
    }
}

// String content (`TransactionError::Description`) is intentionally excluded from the hash
// calculation because user can be tempted to use error description from a third-party libraries
// which aren't stable across the versions.
impl CryptoHash for TransactionResult {
    fn hash(&self) -> Hash {
        u16::hash(&status_as_u16(self))
    }
}

impl From<ExecutionError> for TransactionError {
    fn from(error: ExecutionError) -> Self {
        Self {
            error_type: TransactionErrorType::Code(error.code),
            description: error.description,
        }
    }
}

// `TransactionResult` is stored as `u16` plus `bool` (`true` means that optional part is present)
// with optional string part needed only for string error description.
impl StorageValue for TransactionResult {
    fn into_bytes(self) -> Vec<u8> {
        let mut res = u16::into_bytes(status_as_u16(&self));
        if let Some(description) = self.0.err().and_then(|e| e.description) {
            res.extend(bool::into_bytes(true));
            res.extend(String::into_bytes(description));
        } else {
            res.extend(bool::into_bytes(false));
        }
        res
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let main_part = <u16 as StorageValue>::from_bytes(Cow::Borrowed(&bytes));
        let description = if bool::from_bytes(Cow::Borrowed(&bytes[2..3])) {
            Some(String::from_bytes(Cow::Borrowed(&bytes[3..])))
        } else {
            None
        };

        TransactionResult(match main_part {
            value @ 0...MAX_ERROR_CODE => Err(TransactionError::code(value as u8, description)),
            TRANSACTION_STATUS_OK => Ok(()),
            TRANSACTION_STATUS_PANIC => Err(TransactionError::panic(description)),
            value => panic!("Invalid TransactionResult value: {}", value),
        })
    }
}

fn status_as_u16(status: &TransactionResult) -> u16 {
    match (*status).0 {
        Ok(()) => TRANSACTION_STATUS_OK,
        Err(ref e) => match e.error_type {
            TransactionErrorType::Panic => TRANSACTION_STATUS_PANIC,
            TransactionErrorType::Code(c) => u16::from(c),
        },
    }
}

/// `TransactionSet` trait describes a type which is an `enum` of several transactions.
/// The implementation of this trait is generated automatically by the `transactions!`
/// macro.
pub trait TransactionSet:
    Into<Box<dyn Transaction>> + Clone + Serialize + DeserializeOwned
{
    /// Parses a transaction from this set from a `RawTransaction`.
    fn tx_from_raw(raw: RawTransaction) -> Result<Self, encoding::Error>;
}

/// `transactions!` is used to declare a set of transactions of a particular service.
///
/// The macro generates a type for each transaction and a helper enum which can hold
/// any of the transactions. You need to implement the `Transaction` trait for each of the
/// transactions yourself.
///
/// See [`Service`] trait documentation for a full example of usage.
///
/// Each transaction is specified as a Rust struct. For additional information about
/// data layout, see the documentation on the [`encoding` module](./encoding/index.html).
///
/// For each transaction, the macro creates getter methods for all defined fields.
/// The names of the methods coincide with the field names. In addition,
/// two constructors are defined:
///
/// - `new` accepts as arguments all fields in the order of their declaration in
///   the macro. The constructor returns a transaction which contains
///   the fields. This transaction could be converted into [`Signed<RawTransaction>`]
///
/// Each transaction also implements [`SegmentField`],
/// [`ExonumJson`] and [`StorageValue`] traits for the declared datatype.
///
///
/// **Note.** `transactions!` uses other macros in the `exonum` crate internally.
/// Be sure to add them to the global scope.
///
/// [`Transaction`]: ./blockchain/trait.Transaction.html
/// [parsing]: ./blockchain/trait.Service.html#tymethod.tx_from_raw
/// [`SecretKey`]: ../exonum_crypto/struct.SecretKey.html
/// [`Signature`]: ../exonum_crypto/struct.Signature.html
/// [`SegmentField`]: ./encoding/trait.SegmentField.html
/// [`ExonumJson`]: ./encoding/serialize/json/trait.ExonumJson.html
/// [`StorageValue`]: ./storage/trait.StorageValue.html
/// [`Signed`]: ./messages/struct.Signed.html
/// [`Signed<RawTransaction>`]: ./messages/struct.Signed.html
/// [`Service`]: ./blockchain/trait.Service.html
/// # Examples
///
/// The example below uses the `transactions!` macro; declares a set of
/// transactions for a service with the indicated ID and adds two transactions.
///
/// ```
/// #[macro_use] extern crate exonum;
/// #[macro_use] extern crate serde_derive;
/// use exonum::crypto::PublicKey;
/// # use exonum::storage::Fork;
/// # use exonum::blockchain::{Transaction, ExecutionResult, TransactionContext};
///
/// transactions! {
///     WalletTransactions {
///
///         struct Create {
///             key: &PublicKey
///         }
///
///         struct Transfer {
///             from: &PublicKey,
///             to: &PublicKey,
///             amount: u64,
///         }
///     }
/// }
/// # impl Transaction for Create {
/// #   fn execute(&self, _: TransactionContext) -> ExecutionResult { Ok(()) }
/// # }
/// #
/// # impl Transaction for Transfer {
/// #   fn execute(&self, _: TransactionContext) -> ExecutionResult { Ok(()) }
/// # }
/// #
/// # fn main() { }
/// ```
#[macro_export]
macro_rules! transactions {
    // Empty variant.
    {} => {};
    // Variant with the private enum.
    {
        $(#[$tx_set_attr:meta])*
        $transaction_set:ident {

            $(
                $(#[$tx_attr:meta])*
                struct $name:ident {
                    $($def:tt)*
                }
            )*
        }
    } => {
        messages! {
            $(
                $(#[$tx_attr])*
                struct $name {
                    $($def)*
                }
            )*
        }

        #[derive(Clone, Debug, Serialize, Deserialize)]
        $(#[$tx_set_attr])*
        enum $transaction_set {
            $(
                #[allow(missing_docs)]
                $name($name),
            )*
        }

        transactions!(@implement $transaction_set, $($name)*);
    };
    // Variant with the public enum without restrictions.
    {
        $(#[$tx_set_attr:meta])*
        pub $transaction_set:ident {

            $(
                $(#[$tx_attr:meta])*
                struct $name:ident {
                    $($def:tt)*
                }
            )*
        }
    } => {
        messages! {
            $(
                $(#[$tx_attr])*
                struct $name {
                    $($def)*
                }
            )*
        }

        #[derive(Clone, Debug, Serialize, Deserialize)]
        $(#[$tx_set_attr])*
        pub enum $transaction_set {
            $(
                #[allow(missing_docs)]
                $name($name),
            )*
        }

        transactions!(@implement $transaction_set, $($name)*);
    };
    // Variant with the public enum with visibility restrictions.
    {
        $(#[$tx_set_attr:meta])*
        pub($($vis:tt)+) $transaction_set:ident {

            $(
                $(#[$tx_attr:meta])*
                struct $name:ident {
                    $($def:tt)*
                }
            )*
        }
    } => {
        messages! {
            $(
                $(#[$tx_attr])*
                struct $name {
                    $($def)*
                }
            )*
        }

        #[derive(Clone, Debug, Serialize, Deserialize)]
        $(#[$tx_set_attr])*
        pub($($vis)+) enum $transaction_set {
            $(
                #[allow(missing_docs)]
                $name($name),
            )*
        }

        transactions!(@implement $transaction_set, $($name)*);
    };
    // Implementation details
    (@implement $transaction_set:ident, $($name:ident)*) => {

        impl $crate::blockchain::TransactionSet for $transaction_set {
            fn tx_from_raw(
                raw: $crate::messages::RawTransaction
            ) -> ::std::result::Result<Self, $crate::encoding::Error> {
                let (id, vec) = raw.service_transaction().into_raw_parts();
                __enum_from_id_vec!($transaction_set (id, vec), $( $name )*)
            }
        }

        impl Into<$crate::messages::ServiceTransaction> for $transaction_set {
            fn into(self) -> $crate::messages::ServiceTransaction {
                let (id, vec) = __enum_to_vec_id!($transaction_set &self, $( $name )*);
                $crate::messages::ServiceTransaction::from_raw_unchecked(id, vec)

            }
        }
        $(
        impl Into<$transaction_set> for $name {
            fn into(self) -> $transaction_set {
                $transaction_set::$name(self)
            }
        }

        impl Into<$crate::messages::ServiceTransaction> for $name {
            fn into(self) -> $crate::messages::ServiceTransaction {
                let set: $transaction_set = self.into();
                set.into()
            }
        }
        )*
        impl Into<Box<dyn $crate::blockchain::Transaction>> for $transaction_set {
            fn into(self) -> Box<dyn $crate::blockchain::Transaction> {
                match self {$(
                    $transaction_set::$name(tx) => Box::new(tx),
                )*}
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __enum_to_vec_id{
    ($set:ident $this:expr, $($field_type:ident)*) => {
        __enum_to_vec_id!(@inner $set $this, (0) (); $($field_type)* );
    };
    (@create $set:ident $this:expr, ( $((($name:ident) => ($num:expr)))* ) ) => {
        match $this {
            $(
                &$set::$name(ref tx) => ($num, $crate::messages::BinaryForm::encode(tx).unwrap())
            ),*
        }
    };
    (@inner $set:ident $this:expr, ($num:expr) ($($processed:tt)*);
        $field_type:ident $($rest:tt)*
    ) => {
        __enum_to_vec_id!(
            @inner $set $this,
            ($num + 1)
            ($($processed)* (($field_type) => ($num)));
            $($rest)*
        );
    };

    (@inner $set:ident $this:expr, ($num:expr) ($($processed:tt)*);) => {
        __enum_to_vec_id!(@create $set $this, ($($processed)*) );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __enum_from_id_vec {
    ($set:ident $this:tt, $($field_type:ident)*) => {
        __enum_from_id_vec!(@inner $set $this, (0) (); $($field_type)* );
    };
    (@create $set:ident ($id:expr, $vec:expr), ( $((($name:ident) => ($num:expr)))* ) ) => {
        match $id {
            $(
                num if num == $num => return <$name as $crate::messages::BinaryForm>::decode(&$vec).map($set::$name),
            )*
            num => Err($crate::encoding::Error::Basic(format!("Tag {} not found for enum {}.",num, stringify!($set)).into()))
        }
    };
    (@inner $set:ident $this:tt, ($num:expr) ($($processed:tt)*);
        $field_type:ident $($rest:tt)*
    ) => {
        __enum_from_id_vec!(
            @inner $set $this,
            ($num + 1)
            ($($processed)* (($field_type) => ($num)));
            $($rest)*
        );
    };

    (@inner $set:ident $this:tt, ($num:expr) ($($processed:tt)*);) => {
        __enum_from_id_vec!(@create $set $this, ($($processed)*) );
    };
}

/// Tries to get a meaningful description from the given panic.
fn panic_description(any: &Box<dyn Any + Send>) -> Option<String> {
    if let Some(s) = any.downcast_ref::<&str>() {
        Some(s.to_string())
    } else if let Some(s) = any.downcast_ref::<String>() {
        Some(s.clone())
    } else if let Some(error) = any.downcast_ref::<Box<dyn Error + Send>>() {
        Some(error.description().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use futures::sync::mpsc;

    use std::panic;
    use std::sync::Mutex;

    use super::*;
    use blockchain::{Blockchain, Schema, Service};
    use crypto;
    use encoding;
    use helpers::{Height, ValidatorId};
    use messages::Message;
    use node::ApiSender;
    use storage::{Database, Entry, MemoryDB, Snapshot};

    const TX_RESULT_SERVICE_ID: u16 = 255;

    lazy_static! {
        static ref EXECUTION_STATUS: Mutex<ExecutionResult> = Mutex::new(Ok(()));
    }

    // Testing macro with empty body.
    transactions!{}

    #[test]
    fn execution_error_new() {
        let codes = [0, 1, 100, 255];

        for &code in &codes {
            let error = ExecutionError::new(code);
            assert_eq!(code, error.code);
            assert_eq!(None, error.description);
        }
    }

    #[test]
    fn execution_error_with_description() {
        let values = [(0, ""), (1, "test"), (100, "error"), (255, "hello")];

        for value in &values {
            let error = ExecutionError::with_description(value.0, value.1);
            assert_eq!(value.0, error.code);
            assert_eq!(value.1, error.description.unwrap());
        }
    }

    #[test]
    fn transaction_error_new() {
        let values = [
            (TransactionErrorType::Panic, None),
            (TransactionErrorType::Panic, Some("panic")),
            (TransactionErrorType::Code(0), None),
            (TransactionErrorType::Code(1), Some("")),
            (TransactionErrorType::Code(100), None),
            (TransactionErrorType::Code(255), Some("error description")),
        ];

        for value in &values {
            let error = TransactionError::new(value.0, value.1.map(str::to_owned));
            assert_eq!(value.0, error.error_type());
            assert_eq!(value.1.as_ref().map(|d| d.as_ref()), error.description());
        }
    }

    #[test]
    fn errors_conversion() {
        let execution_errors = [
            ExecutionError::new(0),
            ExecutionError::new(255),
            ExecutionError::with_description(1, ""),
            ExecutionError::with_description(1, "Terrible failure"),
        ];

        for execution_error in &execution_errors {
            let transaction_error: TransactionError = execution_error.clone().into();
            assert_eq!(execution_error.description, transaction_error.description);

            let code = match transaction_error.error_type {
                TransactionErrorType::Code(c) => c,
                _ => panic!("Unexpected transaction error type"),
            };
            assert_eq!(execution_error.code, code);
        }
    }

    #[test]
    fn transaction_results_round_trip() {
        let results = [
            Ok(()),
            Err(TransactionError::panic(None)),
            Err(TransactionError::panic(Some("".to_owned()))),
            Err(TransactionError::panic(Some(
                "Panic error description".to_owned(),
            ))),
            Err(TransactionError::code(0, None)),
            Err(TransactionError::code(
                0,
                Some("Some error description".to_owned()),
            )),
            Err(TransactionError::code(1, None)),
            Err(TransactionError::code(1, Some("".to_owned()))),
            Err(TransactionError::code(100, None)),
            Err(TransactionError::code(100, Some("just error".to_owned()))),
            Err(TransactionError::code(254, None)),
            Err(TransactionError::code(254, Some("e".to_owned()))),
            Err(TransactionError::code(255, None)),
            Err(TransactionError::code(
                255,
                Some("(Not) really long error description".to_owned()),
            )),
        ].iter()
            .map(|res| TransactionResult(res.to_owned()))
            .collect::<Vec<_>>();

        for result in &results {
            let bytes = result.clone().into_bytes();
            let new_result = TransactionResult::from_bytes(Cow::Borrowed(&bytes));
            assert_eq!(*result, new_result);
        }
    }

    #[test]
    fn error_discards_transaction_changes() {
        let statuses = [
            Err(ExecutionError::new(0)),
            Err(ExecutionError::with_description(0, "Strange error")),
            Err(ExecutionError::new(255)),
            Err(ExecutionError::with_description(
                255,
                "Error description...",
            )),
            Ok(()),
        ];

        let (pk, sec_key) = crypto::gen_keypair();
        let mut blockchain = create_blockchain();
        let db = Box::new(MemoryDB::new());

        for (index, status) in statuses.iter().enumerate() {
            let index = index as u64;

            *EXECUTION_STATUS.lock().unwrap() = status.clone();

            let transaction =
                Message::sign_transaction(TxResult::new(index), TX_RESULT_SERVICE_ID, pk, &sec_key);
            let hash = transaction.hash();
            {
                let mut fork = blockchain.fork();
                {
                    let mut schema = Schema::new(&mut fork);
                    schema.add_transaction_into_pool(transaction.clone());
                }
                blockchain.merge(fork.into_patch()).unwrap();
            }

            let (_, patch) = blockchain.create_patch(ValidatorId::zero(), Height(index), &[hash]);

            db.merge(patch).unwrap();

            let mut fork = db.fork();
            let entry = create_entry(&mut fork);
            if status.is_err() {
                assert_eq!(None, entry.get());
            } else {
                assert_eq!(Some(index), entry.get());
            }
        }
    }

    #[test]
    fn str_panic() {
        let static_str = "Static string (&str)";
        let panic = make_panic(static_str);
        assert_eq!(Some(static_str.to_string()), panic_description(&panic));
    }

    #[test]
    fn string_panic() {
        let string = "Owned string (String)".to_owned();
        let error = make_panic(string.clone());
        assert_eq!(Some(string), panic_description(&error));
    }

    #[test]
    fn box_error_panic() {
        let error: Box<dyn Error + Send> = Box::new("e".parse::<i32>().unwrap_err());
        let description = error.description().to_owned();
        let error = make_panic(error);
        assert_eq!(Some(description), panic_description(&error));
    }

    #[test]
    fn unknown_panic() {
        let error = make_panic(1);
        assert_eq!(None, panic_description(&error));
    }

    fn make_panic<T: Send + 'static>(val: T) -> Box<dyn Any + Send> {
        panic::catch_unwind(panic::AssertUnwindSafe(|| panic!(val))).unwrap_err()
    }

    fn create_blockchain() -> Blockchain {
        let service_keypair = crypto::gen_keypair();
        let api_channel = mpsc::channel(1);
        Blockchain::new(
            MemoryDB::new(),
            vec![Box::new(TxResultService) as Box<dyn Service>],
            service_keypair.0,
            service_keypair.1,
            ApiSender::new(api_channel.0),
        )
    }

    struct TxResultService;

    impl Service for TxResultService {
        fn service_id(&self) -> u16 {
            TX_RESULT_SERVICE_ID
        }

        fn service_name(&self) -> &'static str {
            "test service"
        }

        fn state_hash(&self, _: &dyn Snapshot) -> Vec<Hash> {
            vec![]
        }

        fn tx_from_raw(
            &self,
            raw: RawTransaction,
        ) -> Result<Box<dyn Transaction>, encoding::Error> {
            Ok(TestTxs::tx_from_raw(raw)?.into())
        }
    }

    transactions! {
        TestTxs {
            struct TxResult {
                index: u64,
            }
        }
    }

    impl Transaction for TxResult {
        fn execute(&self, mut context: TransactionContext) -> ExecutionResult {
            let mut entry = create_entry(context.fork());
            entry.set(self.index());
            EXECUTION_STATUS.lock().unwrap().clone()
        }
    }

    fn create_entry(fork: &mut Fork) -> Entry<&mut Fork, u64> {
        Entry::new("transaction_status_test", fork)
    }
}
