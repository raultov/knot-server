use crate::handlers::graph_map::*;
use crate::handlers::graph_parse::*;
use crate::handlers::graph_queries::*;
use crate::handlers::graph_utils::*;
use crate::handlers::models::*;
use crate::models::AppState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

#[utoipa::path(
    get,
    path = "/api/repos/{id}/graph",
    tag = "Graph",
    params(
        ("id" = String, Path, description = "Repository ID"),
        GraphParams,
    ),
    responses(
        (status = 200, description = "Graph overview or subgraph results", body = GraphResponse),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository or entity not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Query entity relationship graph. Without entity/entity_id returns an overview; with one returns a subgraph centered on that entity.",
)]
#[tracing::instrument(
    name = "graph",
    skip_all,
    fields(repo_id = %id, depth = params.depth.unwrap_or(2))
)]
pub async fn graph_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphParams>,
) -> Result<Response, Response> {
    if let Some(err) = check_repo_exists(&state, &id) {
        return Err(err);
    }

    let (entity_name, entity_uuid) = resolve_entity(
        &state,
        &id,
        params.entity.as_ref(),
        params.entity_id.as_ref(),
    )
    .await?;

    match entity_name {
        Some(entity_name) => {
            let req =
                SubgraphRequest::from_params(&id, &entity_name, entity_uuid.as_deref(), &params);
            let result = fetch_subgraph(&state, req).await?;

            let mut response = subgraph_to_response(result);
            filter_unconnected_nodes(&mut response);
            Ok((StatusCode::OK, Json(response)).into_response())
        }
        None => {
            let depth = params.depth.unwrap_or(2).clamp(1, 5);
            let rels_str = params
                .relationships
                .as_deref()
                .unwrap_or(DEFAULT_RELATIONSHIPS_OVERVIEW);
            let relationships = match parse_relationships(rels_str) {
                Ok(rels) => rels,
                Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
            };

            let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
            let visible_kinds = match parse_kinds(kinds_str) {
                Ok(kinds) => kinds,
                Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
            };
            let other = includes_other(kinds_str);

            let rel_filter = relationships.join("|");
            let visible_set: std::collections::HashSet<&str> =
                visible_kinds.iter().copied().collect();
            let spec = GraphQuerySpec {
                repo_id: &id,
                depth,
                rel_filter: &rel_filter,
                visible_kinds: &visible_kinds,
                visible_set: &visible_set,
                include_other: other,
            };
            match fetch_all_entities(&state, &spec).await {
                Ok(response) => Ok((StatusCode::OK, Json(response)).into_response()),
                Err(e) => Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Graph overview query failed: {e}"),
                )),
            }
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/graph/expand",
    tag = "Graph",
    params(
        ("id" = String, Path, description = "Repository ID"),
        GraphExpandParams,
    ),
    responses(
        (status = 200, description = "Expanded subgraph results", body = GraphResponse),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository or entity not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Expand entity relationship graph. Returns a subgraph centered on an entity while excluding specified node UUIDs.",
)]
#[tracing::instrument(
    name = "graph_expand",
    skip_all,
    fields(repo_id = %id, depth = params.depth.unwrap_or(2))
)]
pub async fn graph_expand_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphExpandParams>,
) -> Result<Response, Response> {
    if let Some(err) = check_repo_exists(&state, &id) {
        return Err(err);
    }

    let (entity_name, entity_uuid) = resolve_entity(
        &state,
        &id,
        params.entity.as_ref(),
        params.entity_id.as_ref(),
    )
    .await?;

    let entity_name = match entity_name {
        Some(name) => name,
        None => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity' or 'entity_id'",
            ));
        }
    };

    let exclude_uuids: std::collections::HashSet<String> = params
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let req = SubgraphRequest::from_params(&id, &entity_name, entity_uuid.as_deref(), &params);
    let mut result = fetch_subgraph(&state, req).await?;

    if !exclude_uuids.is_empty() {
        result.nodes.retain(|n| !exclude_uuids.contains(&n.uuid));
    }

    let mut response = subgraph_to_response(result);
    filter_unconnected_nodes(&mut response);

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Helper to verify if the repository exists in the registry.
fn check_repo_exists(state: &AppState, id: &str) -> Option<Response> {
    let mut registry = state.registry.lock().unwrap();
    if registry.get(id).is_none() {
        Some(error_response(
            StatusCode::NOT_FOUND,
            format!("Repository '{}' not found", id),
        ))
    } else {
        None
    }
}

/// Resolves the entity name and UUID, validating that the entity exists.
async fn resolve_entity(
    state: &AppState,
    repo_id: &str,
    entity_name: Option<&String>,
    entity_uuid: Option<&String>,
) -> Result<(Option<String>, Option<String>), Response> {
    if let Some(uuid) = entity_uuid
        && !uuid.trim().is_empty()
    {
        return match resolve_uuid_to_name(state, uuid, repo_id).await {
            Ok(Some(name)) => Ok((Some(name), Some(uuid.clone()))),
            Ok(None) => Err(error_response(
                StatusCode::NOT_FOUND,
                format!("Entity with UUID '{}' not found", uuid),
            )),
            Err(e) => Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve entity UUID: {e}"),
            )),
        };
    }

    match entity_name {
        Some(e) if !e.trim().is_empty() => Ok((Some(e.clone()), None)),
        _ => Ok((None, None)),
    }
}

/// Removes nodes from the subgraph that have no connecting edges (except the root node).
fn filter_unconnected_nodes(response: &mut GraphResponse) {
    let connected_uuids: std::collections::HashSet<&str> = response
        .edges
        .iter()
        .flat_map(|e| vec![e.source.as_str(), e.target.as_str()])
        .collect();

    response.nodes.retain(|n| {
        Some(&n.id) == response.root_id.as_ref() || connected_uuids.contains(n.id.as_str())
    });
}

/// Trait unifying query parameters between `GraphParams` and `GraphExpandParams`.
trait CommonGraphParams {
    fn depth(&self) -> Option<u32>;
    fn direction(&self) -> Option<&str>;
    fn relationships(&self) -> Option<&str>;
    fn kinds(&self) -> Option<&str>;
}

impl CommonGraphParams for GraphParams {
    fn depth(&self) -> Option<u32> {
        self.depth
    }
    fn direction(&self) -> Option<&str> {
        self.direction.as_deref()
    }
    fn relationships(&self) -> Option<&str> {
        self.relationships.as_deref()
    }
    fn kinds(&self) -> Option<&str> {
        self.kinds.as_deref()
    }
}

impl CommonGraphParams for GraphExpandParams {
    fn depth(&self) -> Option<u32> {
        self.depth
    }
    fn direction(&self) -> Option<&str> {
        self.direction.as_deref()
    }
    fn relationships(&self) -> Option<&str> {
        self.relationships.as_deref()
    }
    fn kinds(&self) -> Option<&str> {
        self.kinds.as_deref()
    }
}

/// Request-side parameters for a subgraph query before parsing and validation.
///
/// Groups the raw query-string values shared by `GraphParams` and `GraphExpandParams`
/// so `fetch_subgraph` mirrors `SubgraphQueryParams` without requiring a positional
/// argument list that triggers `clippy::too_many_arguments`.
struct SubgraphRequest<'a> {
    repo_id: &'a str,
    entity_name: &'a str,
    entity_uuid: Option<&'a str>,
    depth: Option<u32>,
    direction: Option<&'a str>,
    relationships: Option<&'a str>,
    kinds: Option<&'a str>,
}

impl<'a> SubgraphRequest<'a> {
    fn from_params(
        repo_id: &'a str,
        entity_name: &'a str,
        entity_uuid: Option<&'a str>,
        params: &'a impl CommonGraphParams,
    ) -> Self {
        Self {
            repo_id,
            entity_name,
            entity_uuid,
            depth: params.depth(),
            direction: params.direction(),
            relationships: params.relationships(),
            kinds: params.kinds(),
        }
    }
}

/// Builds the shared subgraph query parameters from request input and runs the query.
///
/// Centralizes parameter parsing (depth, direction, relationships, visible kinds) and
/// the `run_get_subgraph` invocation so both `graph_handler` and `graph_expand_handler`
/// share the exact same code path.
async fn fetch_subgraph(
    state: &AppState,
    req: SubgraphRequest<'_>,
) -> Result<knot::models::SubgraphResult, Response> {
    let depth = req.depth.unwrap_or(2).clamp(1, 5);
    let direction = parse_direction(req.direction.unwrap_or("both"));
    let relationships =
        parse_relationships(req.relationships.unwrap_or(DEFAULT_RELATIONSHIPS_SUBGRAPH))
            .map_err(|msg| error_response(StatusCode::BAD_REQUEST, msg))?;

    let kinds_str = req.kinds.unwrap_or(DEFAULT_VISIBLE_KINDS);
    let visible_kinds =
        parse_kinds(kinds_str).map_err(|msg| error_response(StatusCode::BAD_REQUEST, msg))?;
    let kind_filter: Option<&[&str]> = if includes_other(kinds_str) {
        None
    } else {
        Some(visible_kinds.as_slice())
    };

    knot::cli_tools::run_get_subgraph(
        knot::cli_tools::SubgraphQueryParams {
            entity_name: req.entity_name,
            repo_name: req.repo_id,
            depth,
            relationships: &relationships,
            direction,
            max_nodes: None,
            entity_uuid: req.entity_uuid,
            visible_kinds: kind_filter,
        },
        &state.graph_db,
    )
    .await
    .map_err(|e| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Graph query failed: {e}"),
        )
    })
}
