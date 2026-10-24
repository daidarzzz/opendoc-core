//! Filtro por docset en la consulta (v0.2): `cpp:vector`,
//! `python,django:string`. Puro y testeable: parte el texto crudo en
//! filtros + resto; la resolución contra docsets vive en `service`.

/// Consulta partida por el primer `:` (ver reglas en `parse_query`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Claves en minúsculas y sin vacíos (`["python", "django"]`).
    pub filters: Vec<String>,
    /// Resto tras el `:` con trim (`string`).
    pub query: String,
}

/// Parte `raw` en filtros + consulta:
/// - Sin `:` (o en posición 0, como `:foo`) → todo es consulta.
/// - Si tras el primer `:` viene otro `:` → todo es consulta
///   (`std::vector` NO es filtro).
/// - Cabecera por comas con trim; vacías se ignoran (`a,,b:x`).
/// - La cola conserva `:` internos (`cpp:std::vector` → filtro `cpp`,
///   consulta `std::vector`, que sigue la regla de último segmento de T5).
pub fn parse_query(raw: &str) -> ParsedQuery {
    let plain = || ParsedQuery {
        filters: Vec::new(),
        query: raw.trim().to_string(),
    };
    let Some(colon) = raw.find(':') else {
        return plain();
    };
    if colon == 0 {
        return plain();
    }
    let (head, tail) = raw.split_at(colon);
    let tail = &tail[1..];
    if tail.starts_with(':') {
        return plain();
    }
    let filters: Vec<String> = head
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if filters.is_empty() {
        return plain();
    }
    ParsedQuery {
        filters,
        query: tail.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(raw: &str) -> Vec<String> {
        parse_query(raw).filters
    }

    fn query(raw: &str) -> String {
        parse_query(raw).query
    }

    #[test]
    fn splits_filters_and_query() {
        assert_eq!(filters("cpp:vector"), vec!["cpp"]);
        assert_eq!(query("cpp:vector"), "vector");
        assert_eq!(filters("python,django:string"), vec!["python", "django"]);
        assert_eq!(query("python,django:string"), "string");
        assert_eq!(filters("css:grid"), vec!["css"]);
    }

    #[test]
    fn case_spacing_and_empty_segments() {
        assert_eq!(filters("CPP:Vector"), vec!["cpp"]);
        assert_eq!(query("CPP:Vector"), "Vector");
        assert_eq!(query("cpp: vector"), "vector");
        assert_eq!(filters("python , django:string"), vec!["python", "django"]);
        assert_eq!(filters("cpp,,css:x"), vec!["cpp", "css"]);
        assert_eq!(query("  cpp:vector  "), "vector");
    }

    #[test]
    fn double_colon_is_plain_query() {
        assert!(filters("std::vector").is_empty());
        assert_eq!(query("std::vector"), "std::vector");
        assert_eq!(filters("a:b::c"), vec!["a"]);
        assert_eq!(query("a:b::c"), "b::c");
        assert!(filters(":foo").is_empty());
        assert!(filters("Array#map").is_empty());
        assert!(filters("vector").is_empty());
        assert!(filters("").is_empty());
    }

    #[test]
    fn empty_query_after_filter() {
        assert_eq!(filters("cpp:"), vec!["cpp"]);
        assert_eq!(query("cpp:"), "");
        assert_eq!(query("cpp:   "), "");
    }
}
