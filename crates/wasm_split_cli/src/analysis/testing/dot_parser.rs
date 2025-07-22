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

use std::collections::{HashMap, HashSet};

use crate::analysis::dep_graph::DepNode;
use crate::index::{DataSegmentId, InputFuncId, SymbolId};
use nom::branch::alt;
use nom::bytes::{tag, take_while};
use nom::character::{complete, multispace0};
use nom::Parser;
use nom::{combinator::map_res, IResult};

fn parse_fn_node(input: &str) -> IResult<&str, DepNode> {
    let (input, _) = (tag("F("), multispace0()).parse(input)?;

    let (input, val) = map_res(take_while(|c: char| c.is_digit(10)), |s: &str| {
        s.parse::<InputFuncId>()
    })
    .parse(input)?;

    let (input, _) = (multispace0(), tag(")")).parse(input)?;
    Ok((input, DepNode::Function(val)))
}

fn parse_data_node(input: &str) -> IResult<&str, DepNode> {
    let (input, _) = (tag("D("), multispace0()).parse(input)?;
    let (input, segment) = map_res(take_while(|c: char| c.is_digit(10)), |s: &str| {
        s.parse::<DataSegmentId>()
    })
    .parse(input)?;

    let (input, _) = (multispace0(), tag(","), multispace0()).parse(input)?;

    let (input, symbol) = map_res(take_while(|c: char| c.is_digit(10)), |s: &str| {
        s.parse::<SymbolId>()
    })
    .parse(input)?;
    let (input, _) = (multispace0(), tag(")")).parse(input)?;
    Ok((input, DepNode::DataSymbol(segment, symbol)))
}

fn parse_any_node(input: &str) -> IResult<&str, DepNode> {
    let (input, (_space, dep_node)) =
        (multispace0(), alt((parse_fn_node, parse_data_node))).parse(input)?;
    Ok((input, dep_node))
}

fn parse_arrow(input: &str) -> IResult<&str, ()> {
    let (input, _) = tag("->").parse(input)?;
    Ok((input, ()))
}

fn parse_ampersand(input: &str) -> IResult<&str, ()> {
    let (input, _) = tag("&").parse(input)?;
    Ok((input, ()))
}

fn parse_operator(input: &str) -> IResult<&str, Operator> {
    let (input, (_space, op)) = (multispace0(), alt((tag("->"), tag("&")))).parse(input)?;
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

fn parse_oneline_deps(input: &str) -> IResult<&str, HashMap<DepNode, HashSet<DepNode>>> {
    if input.trim().is_empty() {
        return Ok((input, HashMap::new()));
    }
    let (input, first_node) = parse_any_node(input)?;
    let mut nodes: HashMap<DepNode, HashSet<DepNode>> = HashMap::new();
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
    }

    Ok((input, nodes))
}

pub fn parse_deps(input: &str) -> IResult<&str, HashMap<DepNode, HashSet<DepNode>>> {
    let mut deps: HashMap<DepNode, HashSet<DepNode>> = HashMap::new();

    let mut input = input;
    while !input.is_empty() {
        // Remove leading whitespace and newlines
        let (next_input, val) = nom::bytes::complete::take_while(|c| c != '\n').parse(input)?;

        let (_, nodes) = parse_oneline_deps(val)?;

        for (parent, children) in nodes {
            deps.entry(parent).or_default().extend(children);
        }
        if next_input.is_empty() {
            input = next_input;
            break;
        }
        let (next_input, _) = complete::multispace0(next_input)?;
        input = next_input;
    }

    Ok((input, deps))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_fn() {
        let input = "F(123)";
        let (remaining, dep_node) = super::parse_fn_node(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(dep_node, super::DepNode::Function(123));

        let input = "F( 456 )";
        let (remaining, dep_node) = super::parse_fn_node(input).unwrap();

        assert_eq!(remaining, "");
        assert_eq!(dep_node, super::DepNode::Function(456));
    }

    #[test]
    fn test_parse_dep() {
        let input = "D(789, 1011)";
        let (remaining, dep_node) = super::parse_data_node(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(dep_node, super::DepNode::DataSymbol(789, 1011));
    }

    #[test]
    fn test_simple_dep() {
        let input = "F(1) -> D(2, 3)";
        let (remaining, nodes) = super::parse_oneline_deps(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(nodes, {
            let mut map = std::collections::HashMap::new();
            map.insert(
                super::DepNode::Function(1),
                vec![super::DepNode::DataSymbol(2, 3)].into_iter().collect(),
            );
            map
        });
    }

    #[test]
    fn test_more_deps() {
        let input = "F(1) -> D(2, 3) & F(4) -> D(5, 6)";
        let (remaining, same_nodes) = super::parse_oneline_deps(input).unwrap();
        assert_eq!(remaining, "");
        let (remaining, nodes) = super::parse_deps(input).unwrap();

        assert_eq!(remaining, "");
        assert_eq!(same_nodes, nodes);
        assert_eq!(nodes, {
            let mut map = std::collections::HashMap::new();
            map.insert(
                super::DepNode::Function(1),
                vec![
                    super::DepNode::DataSymbol(2, 3),
                    super::DepNode::Function(4),
                ]
                .into_iter()
                .collect(),
            );
            map.insert(
                super::DepNode::Function(4),
                vec![super::DepNode::DataSymbol(5, 6)].into_iter().collect(),
            );
            map
        });
    }

    #[test]
    fn test_multiline_even_more_deps() {
        let input = r#"
        F(1) -> D(2, 3) & F(4) -> D(5, 6) & F(7) -> D(8, 9)
        F(10) -> D(11, 12)
        F(13) -> F(1) & F(4)
        "#;
        let (remaining, nodes) = super::parse_deps(input).unwrap();
        assert_eq!(remaining, "");
        assert_eq!(nodes, {
            let mut map = std::collections::HashMap::new();

            map.insert(
                super::DepNode::Function(1),
                vec![
                    super::DepNode::DataSymbol(2, 3),
                    super::DepNode::Function(4),
                ]
                .into_iter()
                .collect(),
            );
            map.insert(
                super::DepNode::Function(4),
                vec![
                    super::DepNode::DataSymbol(5, 6),
                    super::DepNode::Function(7),
                ]
                .into_iter()
                .collect(),
            );
            map.insert(
                super::DepNode::Function(7),
                vec![super::DepNode::DataSymbol(8, 9)].into_iter().collect(),
            );
            map.insert(
                super::DepNode::Function(10),
                vec![super::DepNode::DataSymbol(11, 12)]
                    .into_iter()
                    .collect(),
            );
            map.insert(
                super::DepNode::Function(13),
                vec![super::DepNode::Function(1), super::DepNode::Function(4)]
                    .into_iter()
                    .collect(),
            );
            map
        });
    }
}
