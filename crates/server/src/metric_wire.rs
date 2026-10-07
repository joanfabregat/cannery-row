//! Metric response adapters serialize the declared REST DTOs.
use crate::{
    api_contract::{convert, decode, encode},
    api_models,
};
use cannery_core::{
    json::{Document, Node, NodeId, model},
    text,
};
use cannery_metrics::{
    aggregation,
    projection::{Context, Error, PointOut},
    repo::{Query, Summary},
};
use cannery_research::science::Science;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
fn objects(document: &Document, id: NodeId) -> Result<&[NodeId], Error> {
    let Some(Node::Array(items)) = document.node(id) else {
        return Err(Error::Validation);
    };
    if items
        .iter()
        .any(|id| !matches!(document.node(*id), Some(Node::Object(_))))
    {
        return Err(Error::Validation);
    }
    Ok(items)
}
fn mapping<T: serde::de::DeserializeOwned>(
    document: &Document,
    id: NodeId,
    context: Context,
) -> Result<T, Error> {
    Ok(decode(&model::encode_model_mapping(
        document,
        id,
        context.inferred_nesting_budget,
    )?)?)
}
fn mappings<T: serde::de::DeserializeOwned>(
    document: &Document,
    ids: &[NodeId],
    context: Context,
) -> Result<Vec<T>, Error> {
    ids.iter()
        .map(|id| mapping(document, *id, context))
        .collect()
}
fn string(document: &Document, id: NodeId, budget: usize) -> Result<String, Error> {
    text::str_value(document, id, budget)
        .map_err(|_| Error::Value)?
        .as_utf8()
        .ok_or(Error::Value)
}
pub(crate) struct EvidenceContext<'a> {
    summary: &'a Summary,
    query: &'a Query,
    failed_attempts: i64,
    sample_count: BigInt,
}
impl<'a> EvidenceContext<'a> {
    pub(crate) fn new(
        summary: &'a Summary,
        query: &'a Query,
        failed_attempts: i64,
    ) -> Result<Self, Error> {
        let sample_count =
            aggregation::sample_count(Some(&summary.sample_count))?.ok_or(Error::Value)?;
        objects(&summary.controls, summary.controls.root())?;
        Ok(Self {
            summary,
            query,
            failed_attempts,
            sample_count,
        })
    }

    fn model(&self, context: Context) -> Result<api_models::Context, Error> {
        Ok(api_models::Context {
            science_revisions: convert(&self.summary.science_revisions)?,
            split: convert(&self.query.split)?,
            authority: self.query.authority.clone(),
            rows: self.summary.rows,
            measured: self.summary.measured,
            sample_count: self.sample_count.to_i64().ok_or(Error::Value)?,
            failed_attempts: self.failed_attempts,
            controls: mappings(
                &self.summary.controls,
                objects(&self.summary.controls, self.summary.controls.root())?,
                context,
            )?,
        })
    }
}
pub(crate) fn page(
    points: &[PointOut<'_>],
    next_before: Option<i64>,
    evidence: &EvidenceContext<'_>,
    context: Context,
) -> Result<Vec<u8>, Error> {
    Ok(encode(&api_models::MetricsPage {
        items: points
            .iter()
            .map(|point| Ok(decode(&point.bytes(context)?)?))
            .collect::<Result<_, Error>>()?,
        next_before,
        context: evidence.model(context)?,
    })?)
}
pub(crate) fn catalog(
    science: &Science<'_>,
    rendering_budget: usize,
    context: Context,
) -> Result<Vec<u8>, Error> {
    let d = science.content;
    let mut baselines = Vec::new();
    if let Some(id) = d.field(d.root(), "baselines") {
        let Some(Node::Array(ids)) = d.node(id) else {
            return Err(Error::Type);
        };
        for id in ids {
            let description = d
                .field(*id, "description")
                .map(|id| {
                    decode(&model::encode_inferred(
                        d,
                        id,
                        context.inferred_nesting_budget,
                    )?)
                })
                .transpose()?;
            baselines.push(api_models::Baseline {
                id: string(d, d.field(*id, "id").ok_or(Error::Key)?, rendering_budget)?,
                revision: string(
                    d,
                    d.field(*id, "revision").ok_or(Error::Key)?,
                    rendering_budget,
                )?,
                description,
            });
        }
    }
    let mut facets = science.hypothesis_facets().map_err(|_| Error::Value)?;
    facets.sort_by_key(cannery_core::text::TextExt::codepoints);
    let mut group_by = vec!["track".to_owned()];
    group_by.extend(
        facets
            .iter()
            .map(|facet| facet.as_utf8().ok_or(Error::Value))
            .collect::<Result<Vec<_>, Error>>()?,
    );
    let metrics = d
        .field(d.root(), "metrics")
        .map(|id| objects(d, id))
        .transpose()?
        .unwrap_or(&[]);
    Ok(encode(&api_models::CatalogOut {
        science_revision: science.revision.to_i64().ok_or(Error::Value)?,
        metrics: mappings(d, metrics, context)?,
        baselines,
        group_by,
    })?)
}
pub(crate) fn dashboard(
    revision: Option<i32>,
    science_revision: &BigInt,
    views: &Document,
    root: NodeId,
    context: Context,
) -> Result<Vec<u8>, Error> {
    Ok(encode(&api_models::DashboardOut {
        dashboard_revision: revision.map(i64::from),
        science_revision: science_revision.to_i64().ok_or(Error::Value)?,
        derived: revision.is_none(),
        views: mappings(views, objects(views, root)?, context)?,
    })?)
}
#[allow(clippy::too_many_arguments, reason = "Explicit public view projection")]
pub(crate) fn view(
    view: &Document,
    revision: Option<i32>,
    registry: &Document,
    aggregation: Option<&String>,
    series: &[cannery_metrics::series::Series<'_>],
    evidence: &EvidenceContext<'_>,
    truncated: bool,
    warnings: &[String],
    context: Context,
) -> Result<Vec<u8>, Error> {
    Ok(encode(&api_models::ViewOut {
        view: mapping(view, view.root(), context)?,
        dashboard_revision: revision.map(i64::from),
        metric: mapping(registry, registry.root(), context)?,
        aggregation: aggregation
            .map(|text| text.as_utf8().ok_or(Error::Value))
            .transpose()?,
        series: series
            .iter()
            .map(|s| Ok(decode(&s.bytes(context)?)?))
            .collect::<Result<_, Error>>()?,
        context: evidence.model(context)?,
        truncated,
        warnings: warnings.to_vec(),
    })?)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
