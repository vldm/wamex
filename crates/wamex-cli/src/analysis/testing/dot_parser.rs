//! simple implementation of DOT like syntax.
//! It support only simple multiline statements like:
//! - `F(a) -> D(b)` or
//! - `F(a) -> F(b) & D(c) ...`
//!
//! without labels or attributes.
//!
//! `->` represent child relationship, `&` is used to separate multiple children.
//! if `->` is used for item after `&`, it is considered as a child of the last item.
//! First operator cannot be `&`.
//!

use gxhash::{HashMap, HashMapExt, HashSet};
use nom::{
    IResult, Parser,
    branch::alt,
    bytes::{complete::take_while, tag},
    character::complete::multispace0,
    combinator::map_res,
};

use crate::index::SymbolId;

fn parse_symbol_id(input: &str) -> IResult<&str, SymbolId> {
    let (input, val) = map_res(take_while(|c: char| c.is_digit(10)), |s: &str| {
        s.parse::<SymbolId>()
    })
    .parse(input)?;

    Ok((input, val))
}

fn parse_any_node(input: &str) -> IResult<&str, SymbolId> {
    let (input, _space) = multispace0(input)?;
    let (input, dep_node) = parse_symbol_id(input)?;
    let (input, _space) = multispace0(input)?;

    Ok((input, dep_node))
}

fn parse_operator(input: &str) -> IResult<&str, Operator> {
    let (input, (_space, op)) = (multispace0, alt((tag("->"), tag("&")))).parse(input)?;
    let operator = match op {
        "->" => Operator::Arrow,
        "&" => Operator::Ampersand,
        _ => unreachable!(),
    };
    Ok((input, operator))
}

enum Operator {
    Arrow,
    Ampersand,
}

fn parse_oneline_deps(input: &str) -> IResult<&str, HashMap<SymbolId, HashSet<SymbolId>>> {
    if input.trim().is_empty() {
        return Ok((input, HashMap::new()));
    }

    let (input, first_node) = parse_any_node(input)?;
    let mut nodes: HashMap<SymbolId, HashSet<SymbolId>> = HashMap::new();
    let mut last_parrent = first_node;

    let (input, operator) = parse_operator(input)?;
    if matches!(operator, Operator::Ampersand) {
        // first operator cannot be `&`
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    }

    let (input, next_node) = parse_any_node(input)?;
    nodes.entry(last_parrent).or_default().insert(next_node);
    let mut last_child = next_node;

    let mut input = input;
    while !input.is_empty() {
        let (next_input, operator) = parse_operator(input)?;
        input = next_input;

        let (next_input, next_node) = parse_any_node(input)?;
        input = next_input;

        match operator {
            Operator::Arrow => {
                // before pushing next child, make sure to use last child as parent
                last_parrent = last_child;
                nodes.entry(last_parrent).or_default().insert(next_node);
            }
            Operator::Ampersand => {
                nodes.entry(last_parrent).or_default().insert(next_node);
            }
        }
        last_child = next_node;

        let (next_input, _) = multispace0(input)?;
        input = next_input;
    }

    Ok((input, nodes))
}

pub fn parse_deps(input: &str) -> IResult<&str, HashMap<SymbolId, HashSet<SymbolId>>> {
    let mut deps: HashMap<SymbolId, HashSet<SymbolId>> = HashMap::new();

    let mut input = input;
    while !input.is_empty() {
        // Remove leading whitespace and newlines
        let (next_input, val) = take_while(|c| c != '\n').parse(input)?;

        let (_, nodes) = parse_oneline_deps(val)?;

        for (parent, children) in nodes {
            deps.entry(parent).or_default().extend(children);
        }

        let (next_input, _) = multispace0(next_input)?;
        input = next_input;
    }

    Ok((input, deps))
}

pub fn parse_list(input: &str) -> IResult<&str, Vec<SymbolId>> {
    let mut nodes = Vec::new();
    let mut input = input;

    while !input.is_empty() {
        // Remove leading whitespace and newlines
        let (next_input, val) = take_while(|c| c != '\n').parse(input)?;

        let mut line = val;
        while !line.is_empty() {
            let (next_input, node) = parse_any_node(line)?;
            nodes.push(node);

            if next_input.is_empty() {
                break;
            }
            let (next_input, operator) = parse_operator(next_input)?;
            if let Operator::Arrow = operator {
                return Err(nom::Err::Error(nom::error::Error::new(
                    next_input,
                    nom::error::ErrorKind::Tag,
                )));
            }

            line = next_input;
        }

        let (next_input, _) = multispace0(next_input)?;
        input = next_input;
    }

    Ok((input, nodes))
}

#[cfg(test)]
pub mod tests {
    use gxhash::{HashMap, HashMapExt};

    use crate::index::SymbolId;

    pub fn symbol(id: u32) -> super::SymbolId {
        SymbolId::from_index(id)
    }

    #[test]
    fn test_parse_fn() {
        let input = "123";
        let (remaining, dep_node) = super::parse_any_node(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(dep_node, symbol(123));

        let input = " 456 ";
        let (remaining, dep_node) = super::parse_any_node(input).unwrap();

        assert_eq!(remaining, "");
        assert_eq!(dep_node, symbol(456));
    }

    #[test]
    fn test_parse_dep() {
        let input = "789001011";
        let (remaining, dep_node) = super::parse_any_node(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(dep_node, symbol(789001011));
    }

    #[test]
    fn test_simple_dep() {
        let input = "1 -> 2000003";
        let (remaining, nodes) = super::parse_oneline_deps(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(nodes, {
            let mut map = HashMap::new();
            map.insert(symbol(1), vec![symbol(2000003)].into_iter().collect());
            map
        });
    }

    #[test]
    fn test_more_deps() {
        let input = "1 -> 2000003 & 4 -> 5000006";
        let (remaining, same_nodes) = super::parse_oneline_deps(input).unwrap();
        assert_eq!(remaining, "");
        let (remaining, nodes) = super::parse_deps(input).unwrap();

        assert_eq!(remaining, "");
        assert_eq!(same_nodes, nodes);
        assert_eq!(nodes, {
            let mut map = HashMap::new();
            map.insert(
                symbol(1),
                vec![symbol(2000003), symbol(4)].into_iter().collect(),
            );
            map.insert(symbol(4), vec![symbol(5000006)].into_iter().collect());
            map
        });
    }

    #[test]
    fn test_multiline_even_more_deps() {
        let input = r#"
        1 -> 2 & 4 -> 6 & 7 -> 9
        10 -> 11
        13 -> 1 & 4
        "#;
        let (remaining, nodes) = super::parse_deps(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(nodes, {
            let mut map = HashMap::new();

            map.insert(symbol(1), vec![symbol(2), symbol(4)].into_iter().collect());
            map.insert(symbol(4), vec![symbol(6), symbol(7)].into_iter().collect());
            map.insert(symbol(7), vec![symbol(9)].into_iter().collect());
            map.insert(symbol(10), vec![symbol(11)].into_iter().collect());
            map.insert(symbol(13), vec![symbol(1), symbol(4)].into_iter().collect());
            map
        });
    }

    #[test]
    fn test_list() {
        let input = "1 & 2 & 4 & 5";
        let (remaining, nodes) = super::parse_list(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(nodes, vec![symbol(1), symbol(2), symbol(4), symbol(5)]);
    }
}
