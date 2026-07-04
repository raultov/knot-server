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
            let depth = params.depth.unwrap_or(2).clamp(1, 5);
            let direction_str = params.direction.as_deref().unwrap_or("both");
            let direction = parse_direction(direction_str);
            let rels_str = params
                .relationships
                .as_deref()
                .unwrap_or(DEFAULT_RELATIONSHIPS_SUBGRAPH);
            let relationships = match parse_relationships(rels_str) {
                Ok(rels) => rels,
                Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
            };

            let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
            let visible_kinds = match parse_kinds(kinds_str) {
                Ok(kinds) => kinds,
                Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
            };
            let include_oth = includes_other(kinds_str);
            let kind_filter: Option<&[&str]> = if include_oth {
                None
            } else {
                Some(visible_kinds.as_slice())
            };

            match knot::cli_tools::run_get_subgraph(
                &entity_name,
                &id,
                depth,
                &relationships,
                direction,
                None,
                &state.graph_db,
                entity_uuid.as_deref(),
                kind_filter,
            )
            .await
            {
                Ok(result) => {
                    let mut response = subgraph_to_response(result);
                    filter_unconnected_nodes(&mut response);
                    Ok((StatusCode::OK, Json(response)).into_response())
                }
                Err(e) => Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Graph query failed: {e}"),
                )),
            }
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

            match fetch_all_entities(&state, &id, depth, &relationships, &visible_kinds, other)
                .await
            {
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

    let direction_str = params.direction.as_deref().unwrap_or("both");
    let direction = parse_direction(direction_str);
    let rels_str = params
        .relationships
        .as_deref()
        .unwrap_or(DEFAULT_RELATIONSHIPS_SUBGRAPH);
    let relationships = match parse_relationships(rels_str) {
        Ok(rels) => rels,
        Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
    };

    let exclude_uuids: std::collections::HashSet<String> = params
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let depth = params.depth.unwrap_or(2).clamp(1, 5);

    let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
    let visible_kinds = match parse_kinds(kinds_str) {
        Ok(kinds) => kinds,
        Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
    };
    let include_oth = includes_other(kinds_str);
    let kind_filter: Option<&[&str]> = if include_oth {
        None
    } else {
        Some(visible_kinds.as_slice())
    };

    match knot::cli_tools::run_get_subgraph(
        &entity_name,
        &id,
        depth,
        &relationships,
        direction,
        None,
        &state.graph_db,
        entity_uuid.as_deref(),
        kind_filter,
    )
    .await
    {
        Ok(mut result) => {
            if !exclude_uuids.is_empty() {
                result.nodes.retain(|n| !exclude_uuids.contains(&n.uuid));
            }

            let mut response = subgraph_to_response(result);
            filter_unconnected_nodes(&mut response);

            Ok((StatusCode::OK, Json(response)).into_response())
        }
        Err(e) => Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Graph expand failed: {e}"),
        )),
    }
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
