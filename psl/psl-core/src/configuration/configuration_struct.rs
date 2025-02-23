use super::{Datasource, Generator};
use crate::{
    datamodel_connector::RelationMode,
    diagnostics::{DatamodelError, Diagnostics},
    PreviewFeature,
};
use enumflags2::BitFlags;

#[derive(Debug)]
pub struct Configuration {
    pub generators: Vec<Generator>,
    pub datasources: Vec<Datasource>,
    pub warnings: Vec<diagnostics::DatamodelWarning>,
}

impl Configuration {
    pub fn validate_that_one_datasource_is_provided(&self) -> Result<(), Diagnostics> {
        if self.datasources.is_empty() {
            Err(DatamodelError::new_validation_error(
                "You defined no datasource. You must define exactly one datasource.",
                schema_ast::ast::Span::new(0, 0),
            )
            .into())
        } else {
            Ok(())
        }
    }

    pub fn relation_mode(&self) -> Option<RelationMode> {
        self.datasources.first().map(|source| source.relation_mode())
    }

    pub fn max_identifier_length(&self) -> usize {
        self.datasources
            .first()
            .map(|source| source.active_connector.max_identifier_length())
            .unwrap_or(usize::MAX)
    }

    pub fn preview_features(&self) -> BitFlags<PreviewFeature> {
        self.generators.iter().fold(BitFlags::empty(), |acc, generator| {
            acc | generator.preview_features.unwrap_or_default()
        })
    }

    pub fn resolve_datasource_urls_from_env<F>(
        &mut self,
        url_overrides: &[(String, String)],
        env: F,
    ) -> Result<(), Diagnostics>
    where
        F: Fn(&str) -> Option<String> + Copy,
    {
        for datasource in &mut self.datasources {
            if let Some((_, url)) = url_overrides.iter().find(|(name, _url)| name == &datasource.name) {
                datasource.url.value = Some(url.clone());
                datasource.url.from_env_var = None;
            }

            if datasource.url.from_env_var.is_some() && datasource.url.value.is_none() {
                datasource.url.value = Some(datasource.load_url(env)?);
            }

            if let (Some(direct_url), Some(span)) = (&datasource.direct_url, &datasource.direct_url_span) {
                let result = match super::from_url(direct_url, env) {
                        Err(err) => {
                            match err {
                        super::UrlValidationError::EmptyUrlValue => {
                            let msg = "You must provide a nonempty direct URL";
                            Err(DatamodelError::new_source_validation_error(msg, &datasource.name, *span))
                        }
                        super::UrlValidationError::EmptyEnvValue(env_var) => {
                            Err(DatamodelError::new_source_validation_error(
                                &format!(
                        "You must provide a nonempty direct URL. The environment variable `{}` resolved to an empty string.",
                        env_var
                    ),
                                &datasource.name,
                                *span,
                            ))
                        },
                        super::UrlValidationError::NoEnvValue(env_var) => {
                            Err(DatamodelError::new_environment_functional_evaluation_error(
                                env_var,
                                *span,
                            ))
                        },
                        super::UrlValidationError::NoUrlOrEnv => {
                          Ok(None)
                        },
                    }
                        },
                        Ok(res) => Ok(Some(res)),
                    }?;
                datasource.direct_url = Some(crate::StringFromEnvVar {
                    from_env_var: direct_url.from_env_var.clone(),
                    value: result,
                });
            }
        }

        Ok(())
    }
}
