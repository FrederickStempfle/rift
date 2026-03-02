/// Simple route pattern matcher for Rift function routes.
///
/// Supports exact segments (`/api/hello`), parameterized segments
/// (`/api/users/:id`), and root (`/`).
///
/// Check if a route pattern matches a URL path.
pub fn route_matches(pattern: &str, path: &str) -> bool {
    if pattern == "/" {
        return path == "/";
    }

    let pattern_segs: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if pattern_segs.len() != path_segs.len() {
        return false;
    }

    pattern_segs
        .iter()
        .zip(path_segs.iter())
        .all(|(p, s)| p.starts_with(':') || *p == *s)
}

/// Extract named parameters from a matched route.
///
/// Returns `None` if the pattern doesn't match the path.
pub fn extract_route_params(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
    if !route_matches(pattern, path) {
        return None;
    }

    if pattern == "/" {
        return Some(Vec::new());
    }

    let pattern_segs: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut params = Vec::new();
    for (p, s) in pattern_segs.iter().zip(path_segs.iter()) {
        if let Some(name) = p.strip_prefix(':') {
            params.push((name.to_string(), s.to_string()));
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(route_matches("/api/hello", "/api/hello"));
    }

    #[test]
    fn exact_no_match() {
        assert!(!route_matches("/api/hello", "/api/world"));
    }

    #[test]
    fn root_match() {
        assert!(route_matches("/", "/"));
    }

    #[test]
    fn root_no_match() {
        assert!(!route_matches("/", "/api"));
    }

    #[test]
    fn param_match() {
        assert!(route_matches("/api/users/:id", "/api/users/123"));
    }

    #[test]
    fn param_no_match_length() {
        assert!(!route_matches("/api/users/:id", "/api/users/123/posts"));
    }

    #[test]
    fn multi_param() {
        assert!(route_matches("/api/:org/:repo", "/api/acme/widgets"));
    }

    #[test]
    fn nested_with_param() {
        assert!(route_matches(
            "/api/v1/users/:id/posts",
            "/api/v1/users/42/posts"
        ));
    }

    #[test]
    fn extract_no_params() {
        let params = extract_route_params("/api/hello", "/api/hello").unwrap();
        assert!(params.is_empty());
    }

    #[test]
    fn extract_single_param() {
        let params = extract_route_params("/api/users/:id", "/api/users/42").unwrap();
        assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn extract_multi_params() {
        let params = extract_route_params("/api/:org/:repo", "/api/acme/widgets").unwrap();
        assert_eq!(
            params,
            vec![
                ("org".to_string(), "acme".to_string()),
                ("repo".to_string(), "widgets".to_string()),
            ]
        );
    }

    #[test]
    fn extract_returns_none_on_mismatch() {
        assert!(extract_route_params("/api/hello", "/api/world").is_none());
    }

    #[test]
    fn extract_root() {
        let params = extract_route_params("/", "/").unwrap();
        assert!(params.is_empty());
    }
}
