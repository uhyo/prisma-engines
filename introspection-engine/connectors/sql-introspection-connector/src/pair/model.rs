use psl::{
    datamodel_connector::walker_ext_traits::IndexWalkerExt,
    parser_database::walkers,
    schema_ast::ast::{self, WithDocumentation},
};
use sql_schema_describer as sql;
use std::borrow::Cow;

use super::{IdPair, IndexPair, Pair, RelationFieldDirection, RelationFieldPair, ScalarFieldPair};

pub(crate) type ModelPair<'a> = Pair<'a, walkers::ModelWalker<'a>, sql::TableWalker<'a>>;

impl<'a> ModelPair<'a> {
    /// The position of the model from the PSL, if existing. Used for
    /// sorting the models in the final introspected data model.
    pub(crate) fn previous_position(self) -> Option<ast::ModelId> {
        self.previous.map(|m| m.id)
    }

    /// Temporary method for relations. Eventually we'll remove this
    /// when we handle relations together with models and fields.
    pub(crate) fn table_id(self) -> sql::TableId {
        self.next.id
    }

    /// The namespace of the model, if using the multi-schema feature.
    pub(crate) fn namespace(self) -> Option<&'a str> {
        if self.context.uses_namespaces() {
            self.next.namespace()
        } else {
            None
        }
    }

    /// Name of the model in the PSL. The value can be sanitized if it
    /// contains characters that are not allowed in the PSL
    /// definition.
    pub(crate) fn name(self) -> Cow<'a, str> {
        self.context.table_prisma_name(self.next.id).prisma_name()
    }

    /// The mapped name, if defined, is the actual name of the model in
    /// the database.
    pub(crate) fn mapped_name(self) -> Option<&'a str> {
        self.context.table_prisma_name(self.next.id).mapped_name()
    }

    /// True, if the name of the model is using a reserved identifier.
    pub(crate) fn uses_reserved_name(self) -> bool {
        psl::is_reserved_type_name(self.next.name())
    }

    /// The documentation on top of the enum.
    pub(crate) fn documentation(self) -> Option<&'a str> {
        self.previous.and_then(|model| model.ast_model().documentation())
    }

    /// Iterating over the scalar fields.
    pub(crate) fn scalar_fields(self) -> impl ExactSizeIterator<Item = ScalarFieldPair<'a>> {
        self.next.columns().map(move |next| {
            let previous = self.context.existing_scalar_field(next.id);
            Pair::new(self.context, previous, next)
        })
    }

    /// Iterating over the relation fields.
    pub(crate) fn relation_fields(self) -> Box<dyn Iterator<Item = RelationFieldPair<'a>> + 'a> {
        if self.context.foreign_keys_enabled() {
            let inline = self
                .context
                .inline_relations_for_table(self.table_id())
                .map(move |(direction, fk)| {
                    let previous = self
                        .context
                        .existing_inline_relation(fk.id)
                        .and_then(|rel| match direction {
                            RelationFieldDirection::Forward => rel.forward_relation_field(),
                            RelationFieldDirection::Back => rel.back_relation_field(),
                        });

                    RelationFieldPair::inline(self.context, previous, fk, direction)
                });

            let m2m = self
                .context
                .m2m_relations_for_table(self.table_id())
                .map(move |(direction, next)| RelationFieldPair::m2m(self.context, next, direction));

            Box::new(inline.chain(m2m))
        } else {
            match self.previous {
                Some(prev) => {
                    let fields = prev
                        .relation_fields()
                        .filter(move |rf| !self.context.table_missing_for_model(&rf.related_model().id))
                        .map(move |previous| RelationFieldPair::emulated(self.context, previous));

                    Box::new(fields)
                }
                None => Box::new(std::iter::empty()),
            }
        }
    }

    /// True, if the user has explicitly mapped the model's name in
    /// the PSL.
    pub(crate) fn remapped_name(self) -> bool {
        self.previous.filter(|m| m.mapped_name().is_some()).is_some()
    }

    /// A model must have either a primary key, or at least one unique
    /// index defined that consists of columns that are all supported by
    /// prisma and not null.
    pub(crate) fn has_usable_identifier(self) -> bool {
        self.next
            .indexes()
            .filter(|idx| idx.is_primary_key() || idx.is_unique())
            .any(|idx| {
                idx.columns().all(|c| {
                    !matches!(
                        c.as_column().column_type().family,
                        sql::ColumnTypeFamily::Unsupported(_)
                    ) && c.as_column().arity().is_required()
                })
            })
    }

    /// True, if the model uses the same name as another top-level item from
    /// a different namespace.
    pub(crate) fn uses_duplicate_name(self) -> bool {
        self.previous.is_none() && !self.context.name_is_unique(self.next.name())
    }

    /// If the model is marked as ignored. Can happen either if user
    /// explicitly sets the model attribute, or if the model has no
    /// usable identifiers.
    pub(crate) fn ignored(self) -> bool {
        let explicit_ignore = self.previous.map(|model| model.is_ignored()).unwrap_or(false);
        let implicit_ignore = !self.has_usable_identifier() && self.scalar_fields().len() > 0;

        explicit_ignore || implicit_ignore
    }

    /// Returns an iterator over all indexes of the model,
    /// specifically the ones defined in the model level, skipping the
    /// primary key and unique index defined in a field.
    ///
    /// For the primary key, use [`ModelPair#id`]. For a field-level
    /// unique, use [`ScalarFieldPair#unique`].
    pub(crate) fn indexes(self) -> impl Iterator<Item = IndexPair<'a>> {
        self.next
            .indexes()
            .filter(|i| !(i.is_unique() && i.columns().len() == 1))
            .filter(|i| !i.is_primary_key())
            .map(move |next| {
                let previous = self.previous.and_then(|prev| {
                    prev.indexes().find(|idx| {
                        // Upgrade logic. Prior to Prisma 3, PSL index attributes had a `name` argument but no `map`
                        // argument. If we infer that an index in the database was produced using that logic, we
                        // match up the existing index.
                        if idx.mapped_name().is_none() && idx.name() == Some(next.name()) {
                            return true;
                        }

                        // Compare the constraint name (implicit or mapped name) from the Prisma schema with the
                        // constraint name from the database.
                        idx.constraint_name(self.context.active_connector()) == next.name()
                    })
                });

                Pair::new(self.context, previous, next)
            })
    }

    /// The primary key of the model, if defined. It will only return
    /// a value, if the field should be defined in a model as `@@id`:
    /// e.g. when it holds more than one field.
    pub(crate) fn id(self) -> Option<IdPair<'a>> {
        self.next
            .primary_key()
            .filter(|pk| pk.columns().len() > 1)
            .and_then(move |pk| {
                let id = self.previous.and_then(|model| model.primary_key());
                let pair = Pair::new(self.context, id, pk);

                (!pair.defined_in_a_field()).then_some(pair)
            })
    }
}
