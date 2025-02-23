//! Intermediate representation of the input document that is used by the query engine to build
//! query ASTs and validate the incoming data.
//!
//! Helps decoupling the incoming protocol layer from the query engine, i.e. allows the query engine
//! to be agnostic to the actual protocol that is used on upper layers, as long as they translate
//! to this simple intermediate representation.
//!
//! The mapping illustrated with GraphQL (GQL):
//! - There can be multiple queries and/or mutations in one GQL request, usually designated by "query / {}" or "mutation".
//! - Inside the queries / mutations are fields in GQL. In Prisma, every one of those designates exactly one `Operation` with a `Selection`.
//! - Operations are broadly divided into reading (query in GQL) or writing (mutation).
//! - The field that designates the `Operation` pretty much exactly maps to a `Selection`:
//!    - It can have arguments,
//!    - it can be aliased,
//!    - it can have a number of nested selections (selection set in GQL).
//! - Arguments contain concrete values and complex subtypes that are parsed and validated by the query builders, and then used for querying data (input types in GQL).
mod error;
mod operation;
mod parse_ast;
mod parser;
mod selection;
mod transformers;

pub use error::*;
pub use operation::*;
pub use parse_ast::*;
pub use parser::*;
pub use selection::*;
pub use transformers::*;

use crate::resolve_compound_field;
use prisma_models::{ModelRef, PrismaValue};
use schema::QuerySchemaRef;
use schema_builder::constants::*;
use std::collections::HashMap;

pub type QueryParserResult<T> = std::result::Result<T, QueryParserError>;

#[derive(Debug)]
pub enum QueryDocument {
    Single(Operation),
    Multi(BatchDocument),
}

impl QueryDocument {
    pub fn dedup_operations(self) -> Self {
        match self {
            Self::Single(operation) => Self::Single(operation.dedup_selections()),
            _ => self,
        }
    }
}

#[derive(Debug)]
pub enum BatchDocument {
    Multi(Vec<Operation>, Option<BatchDocumentTransaction>),
    Compact(CompactedDocument),
}

impl BatchDocument {
    pub fn new(operations: Vec<Operation>, transaction: Option<BatchDocumentTransaction>) -> Self {
        Self::Multi(operations, transaction)
    }

    /// Returns true if the operation contains any filters to prevents us from compacting the batch.
    /// Some filters can prevent us (or make it very hard) from mapping the findMany result back to the original findUnique queries.
    ///
    /// Those filters are:
    /// - non scalar filters (ie: relation filters, boolean operators...)
    /// - any scalar filters that is not `EQUALS`
    fn invalid_compact_filter(op: &Operation, schema: &QuerySchemaRef) -> bool {
        if !op.is_find_unique(schema) {
            return true;
        }

        let where_obj = op.as_read().unwrap().arguments()[0].1.clone().into_object().unwrap();
        let field = schema.find_query_field(op.name()).unwrap();
        let model = field.model().unwrap();

        where_obj.iter().any(|(key, val)| match val {
            // If it's a compound, then it's still considered as scalar
            PrismaValue::Object(_) if resolve_compound_field(key, model).is_some() => false,
            // Otherwise, we just look for a scalar field inside the model. If it's not one, then we break.
            val => match model.fields().find_from_scalar(&key) {
                Ok(_) => match val {
                    // Consider scalar _only_ if the filter object contains "equals". eg: `{ scalar_field: { equals: 1 } }`
                    PrismaValue::Object(obj) => !obj.iter().any(|(k, _)| k.as_str() == filters::EQUALS),
                    _ => false,
                },
                Err(_) => true,
            },
        })
    }

    /// Checks whether a BatchDocument can be compacted.
    fn can_compact(&self, schema: &QuerySchemaRef) -> bool {
        match self {
            Self::Multi(operations, _) => match operations.split_first() {
                Some((first, rest)) if first.is_find_unique(schema) => {
                    // If any of the operation has an "invalid" compact filter (see documentation of `invalid_compact_filter`),
                    // we do not compact the queries.
                    let has_invalid_compact_filter =
                        operations.iter().any(|op| Self::invalid_compact_filter(op, schema));

                    if has_invalid_compact_filter {
                        return false;
                    }

                    rest.iter().all(|op| {
                        op.is_find_unique(schema)
                            && first.name() == op.name()
                            && first.nested_selections().len() == op.nested_selections().len()
                            && first
                                .nested_selections()
                                .iter()
                                .all(|fop| op.nested_selections().contains(fop))
                    })
                }
                _ => false,
            },
            Self::Compact(_) => false,
        }
    }

    pub fn compact(self, schema: &QuerySchemaRef) -> Self {
        match self {
            Self::Multi(operations, _) if self.can_compact(schema) => {
                Self::Compact(CompactedDocument::from_operations(operations, schema))
            }
            _ => self,
        }
    }

    /// Returns `true` if the batch document is [`Compact`].
    #[must_use]
    pub fn is_compact(&self) -> bool {
        matches!(self, Self::Compact(..))
    }
}

#[derive(Debug)]
pub struct BatchDocumentTransaction {
    isolation_level: Option<String>,
}

impl BatchDocumentTransaction {
    pub fn new(isolation_level: Option<String>) -> Self {
        Self { isolation_level }
    }

    pub fn isolation_level(&self) -> Option<String> {
        self.isolation_level.clone()
    }
}

#[derive(Debug, Clone)]
pub struct CompactedDocument {
    pub arguments: Vec<HashMap<String, PrismaValue>>,
    pub nested_selection: Vec<String>,
    pub operation: Operation,
    pub keys: Vec<String>,
    name: String,
}

impl CompactedDocument {
    pub fn single_name(&self) -> String {
        format!("findUnique{}", self.name)
    }

    pub fn plural_name(&self) -> String {
        format!("findMany{}", self.name)
    }

    /// Here be the dragons. Ay caramba!
    pub fn from_operations(ops: Vec<Operation>, schema: &QuerySchemaRef) -> Self {
        let field = schema.find_query_field(ops.first().unwrap().name()).unwrap();
        let model = field.model().unwrap();
        // Unpack all read queries (an enum) into a collection of selections.
        // We already took care earlier that all operations here must be reads.
        let selections: Vec<Selection> = ops
            .into_iter()
            .map(|op| op.into_read().expect("Trying to compact a write operation."))
            .collect();

        // This block creates the findMany query from the separate findUnique queries.
        let selection = {
            // The name of the query should be findManyX if the first query
            // here is findUniqueX. We took care earlier the queries are all the
            // same. Otherwise we fail hard here.
            let mut builder = Selection::with_name(selections[0].name().replacen("findUnique", "findMany", 1));

            // Take the nested selection set from the first query. We took care
            // earlier that all the nested selections are the same in every
            // query. Otherwise we fail hard here.
            builder.set_nested_selections(selections[0].nested_selections().to_vec());

            // The query arguments are extracted here. Combine all query
            // arguments from the different queries into a one large argument.
            let selection_set = selections.iter().fold(SelectionSet::new(), |mut acc, selection| {
                // findUnique always has only one argument. We know it must be an object, otherwise this will panic.
                let where_obj = selection.arguments()[0]
                    .1
                    .clone()
                    .into_object()
                    .expect("Trying to compact a selection with non-object argument");
                let filters = extract_filter(where_obj, model);

                for (field, filter) in filters {
                    acc = acc.push(field, filter);
                }

                acc
            });

            // We must select all unique fields in the query so we can
            // match the right response back to the right request later on.
            for key in selection_set.keys() {
                if !builder.contains_nested_selection(key) {
                    builder.push_nested_selection(Selection::with_name(key));
                }
            }

            // The `In` handles both cases, with singular id it'll do an `IN`
            // expression and with a compound id a combination of `AND` and `OR`.
            builder.push_argument(args::WHERE, In::new(selection_set));

            builder.set_alias(selections[0].alias().clone());

            builder
        };

        // We want to store the original nested selections so we can filter out
        // the added unique selections from the responses if the original
        // selection set didn't have them.
        let nested_selection = selections[0]
            .nested_selections()
            .iter()
            .map(|s| s.name().to_string())
            .collect();

        // Saving the stub of the query name for later use.
        let name = selections[0].name().replacen("findUnique", "", 1);

        // Convert the selections into a map of arguments. This defines the
        // response order and how we fetch the right data from the response set.
        let arguments: Vec<HashMap<String, PrismaValue>> = selections
            .into_iter()
            .map(|mut sel| {
                let where_obj = sel.pop_argument().unwrap().1.into_object().unwrap();
                let filter_map: HashMap<String, PrismaValue> = extract_filter(where_obj, model).into_iter().collect();

                filter_map
            })
            .collect();

        // Gets the argument keys for later mapping.
        let keys: Vec<_> = arguments[0]
            .iter()
            .flat_map(|pair| match pair {
                (_, PrismaValue::Object(obj)) => obj.iter().map(|(key, _)| key.to_owned()).collect(),
                (key, _) => vec![key.to_owned()],
            })
            .collect();

        Self {
            name,
            arguments,
            nested_selection,
            keys,
            operation: Operation::Read(selection),
        }
    }
}

/// Takes in a unique filter, extract the scalar filters and return a simple list of field/filter.
/// This list is used to build a findMany query from multiple findUnique queries.
/// Therefore, compound unique filters are walked and each individual field is added. eg:
/// { field1_field2: { field1: 1, field2: 2 } } -> [(field1, 1), (field2, 2)]
/// This is because findMany filters don't have the special compound unique syntax.
///
/// Furthermore, this list is used to match the results of the findMany query back to the original findUnique queries.
/// Consequently, we only extract EQUALS filters or else we would have to manually implement other filters.
/// This is a limitation that _could_ technically be lifted but that's not worth it for now.
fn extract_filter(where_obj: Vec<SelectionArgument>, model: &ModelRef) -> Vec<SelectionArgument> {
    where_obj
        .into_iter()
        .flat_map(|(key, val)| match val {
            // This means our query has a compound field in the form of: {co1_col2: { col1_col2: { col1: <val>, col2: <val> } }}
            PrismaValue::Object(obj) if resolve_compound_field(&key, model).is_some() => obj.into_iter().collect(),
            // This means our query has a scalar filter in the form of {col1: { equals: <val> }}
            PrismaValue::Object(obj) => {
                // This is safe because it's been validated before in the `.can_compact` method.
                let (_, equal_val) = obj
                    .iter()
                    .find(|(k, _)| k == filters::EQUALS)
                    .expect("we only support scalar equals filters");

                vec![(key, equal_val.clone())]
            }
            // ...or a singular argument in a form of {col1: <val>}
            x => {
                vec![(key, x)]
            }
        })
        .collect()
}
