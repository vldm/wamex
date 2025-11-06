use std::str::FromStr;

use nom::{
    IResult, Parser,
    branch::alt,
    bytes::complete::{tag, take_until, take_while1},
    character::complete::{char, digit1, multispace0, multispace1},
    combinator::{map, map_res, opt, recognize},
    multi::{many0, separated_list0},
    sequence::{delimited, preceded, terminated},
};

use crate::{DemangledName, SymbolSignature, Type};

// Nom parsers for SymbolSignature
fn parse_type(input: &str) -> IResult<&str, Type> {
    use nom::Parser;
    alt((
        map(tag("I32"), |_| Type::I32),
        map(tag("I64"), |_| Type::I64),
        map(tag("F32"), |_| Type::F32),
        map(tag("F64"), |_| Type::F64),
        map(tag("V128"), |_| Type::V128),
        map(tag("FuncRef"), |_| Type::FuncRef),
        map(tag("ExternRef"), |_| Type::ExternRef),
    ))
    .parse(input)
}

// Parse any input before outstanding 'ending' char
// If '<' is found, it will ignore 'ending' unless closing '>' is also found
fn parse_identifier(input: &str, ending: char) -> IResult<&str, &str> {
    // Parse content inside angle brackets (including nested brackets)
    let angle_bracket_content = delimited(char('<'), take_until(">"), char('>'));

    // Parse identifier part: either regular characters or angle bracket content
    let identifier_part = alt((
        angle_bracket_content,
        take_while1(|c: char| c != '<' && c != ending),
    ));

    // Combine multiple parts into a single identifier
    let mut full_identifier = recognize(many0(identifier_part));

    // Parse until we hit 'ending' that's not inside angle brackets
    let (input, name) = full_identifier.parse(input)?;

    // Trim any trailing whitespace
    let name = name.trim_end();

    Ok((input, name))
}

fn parse_function_signature(input: &str) -> IResult<&str, SymbolSignature> {
    use nom::Parser;

    let (input, lazy) = opt(terminated(tag("lazy"), multispace1)).parse(input)?;
    let (input, _) = tag("fn").parse(input)?;
    let (input, _) = multispace1(input)?;

    let (input, name) = parse_identifier(input, '(')?;
    let (input, _) = tag("(")(input)?;
    let (input, params) =
        separated_list0((multispace0, char(','), multispace0), parse_type).parse(input)?;
    let (input, _) = tag(")")(input)?;

    let (input, results) = opt(preceded(
        (multispace0, tag("->"), multispace0),
        separated_list0((multispace0, char(','), multispace0), parse_type),
    ))
    .parse(input)?;

    Ok((
        input,
        SymbolSignature::Function {
            name: DemangledName {
                name: name.to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            lazy: lazy.is_some(),
            params,
            results: results.unwrap_or_default(),
        },
    ))
}

fn parse_data_signature(input: &str) -> IResult<&str, SymbolSignature> {
    use nom::Parser;

    let (input, _) = tag("data").parse(input)?;
    let (input, _) = multispace1(input)?;
    let (input, _) = tag("(")(input)?;

    let (input, name) = parse_identifier(input, ')')?;
    let (input, _) = tag(")")(input)?;
    let (input, _) = char(':')(input)?;
    let (input, size) = map_res(digit1, |s: &str| s.parse::<u32>()).parse(input)?;

    Ok((
        input,
        SymbolSignature::Data {
            name: DemangledName {
                name: name.to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            size,
        },
    ))
}

fn parse_symbol_signature(input: &str) -> IResult<&str, SymbolSignature> {
    use nom::Parser;
    alt((parse_function_signature, parse_data_signature)).parse(input)
}

impl FromStr for SymbolSignature {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parse_symbol_signature(s) {
            Ok((remaining, signature)) => {
                if remaining.trim().is_empty() {
                    Ok(signature)
                } else {
                    Err(format!("Unexpected remaining input: '{}'", remaining))
                }
            }
            Err(e) => Err(format!("Parse error: {}", e)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_and_parse_function_signature() {
        let signature = SymbolSignature::Function {
            name: DemangledName {
                name: "test_func".to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            lazy: false,
            params: vec![Type::I32, Type::F64],
            results: vec![Type::I32],
        };

        let display = signature.display_signature();
        assert_eq!(display, "fn test_func(I32, F64) -> I32");

        let parsed: SymbolSignature = display.parse().unwrap();
        match parsed {
            SymbolSignature::Function {
                name,
                lazy,
                params,
                results,
            } => {
                assert_eq!(name.name, "test_func");
                assert!(!lazy);
                assert_eq!(params, vec![Type::I32, Type::F64]);
                assert_eq!(results, vec![Type::I32]);
            }
            _ => panic!("Expected function signature"),
        }
    }

    #[test]
    fn test_display_and_parse_lazy_function_signature() {
        let signature = SymbolSignature::Function {
            name: DemangledName {
                name: "lazy_func".to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            lazy: true,
            params: vec![],
            results: vec![],
        };

        let display = signature.display_signature();
        assert_eq!(display, "lazy fn lazy_func()");

        let parsed: SymbolSignature = display.parse().unwrap();
        match parsed {
            SymbolSignature::Function {
                name,
                lazy,
                params,
                results,
            } => {
                assert_eq!(name.name, "lazy_func");
                assert!(lazy);
                assert!(params.is_empty());
                assert!(results.is_empty());
            }
            _ => panic!("Expected function signature"),
        }
    }

    #[test]
    fn test_display_and_parse_data_signature() {
        let signature = SymbolSignature::Data {
            name: DemangledName {
                name: "my_data".to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            size: 42,
        };

        let display = signature.display_signature();
        assert_eq!(display, "data (my_data):42");

        let parsed: SymbolSignature = display.parse().unwrap();
        match parsed {
            SymbolSignature::Data { name, size } => {
                assert_eq!(name.name, "my_data");
                assert_eq!(size, 42);
            }
            _ => panic!("Expected data signature"),
        }
    }

    #[test]
    fn test_valid_names_with_colons_and_commas() {
        let signature = SymbolSignature::Function {
            name: DemangledName {
                name: "std::vector::push,pop".to_string(),
                distinguishing_hash: "hash:value".to_string(),
                anonymous: false,
            },
            lazy: false,
            params: vec![],
            results: vec![],
        };

        let display = signature.display_signature();
        assert_eq!(display, "fn std::vector::push,pop()");

        let parsed: SymbolSignature = display.parse().unwrap();
        match parsed {
            SymbolSignature::Function {
                name,
                lazy,
                params,
                results,
            } => {
                assert_eq!(name.name, "std::vector::push,pop");
                assert!(!lazy);
                assert!(params.is_empty());
                assert!(results.is_empty());
            }
            _ => panic!("Expected function signature"),
        }
    }

    #[test]
    fn test_invalid_name_validation() {
        let signature = SymbolSignature::Function {
            name: DemangledName {
                name: "func(with)invalid<chars>".to_string(),
                distinguishing_hash: String::new(),
                anonymous: false,
            },
            lazy: false,
            params: vec![],
            results: vec![],
        };

        // This should panic due to invalid characters
        let result = std::panic::catch_unwind(|| signature.display_signature());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_for_display_errors() {
        let name_with_parens = DemangledName {
            name: "func<(with_parens)>".to_string(),
            distinguishing_hash: String::new(),
            anonymous: false,
        };
        assert!(name_with_parens.validate_name_spec_symbols().is_ok());

        let name_with_parens = DemangledName {
            name: "func(with_parens)".to_string(),
            distinguishing_hash: String::new(),
            anonymous: false,
        };
        assert!(name_with_parens.validate_name_spec_symbols().is_err());

        let name_with_space = DemangledName {
            name: "func with space".to_string(),
            distinguishing_hash: String::new(),
            anonymous: false,
        };
        assert!(name_with_space.validate_name_spec_symbols().is_ok());

        let hash_with_invalid_chars = DemangledName {
            name: "valid_name".to_string(),
            distinguishing_hash: "hash(with_parens)".to_string(),
            anonymous: false,
        };
        assert!(hash_with_invalid_chars.validate_name_spec_symbols().is_ok());

        // Valid cases should work
        let valid_name = DemangledName {
            name: "std::vector::push,pop".to_string(),
            distinguishing_hash: "hash:value".to_string(),
            anonymous: false,
        };
        assert!(valid_name.validate_name_spec_symbols().is_ok());
    }

    #[test]
    fn test_parse_error_handling() {
        assert!("invalid signature".parse::<SymbolSignature>().is_err());
        assert!("fn incomplete(".parse::<SymbolSignature>().is_err());
        assert!("data (incomplete".parse::<SymbolSignature>().is_err());
    }

    #[test]
    fn test_distinguishing_hash_not_in_signature() {
        let signature = SymbolSignature::Function {
            name: DemangledName {
                name: "my_function".to_string(),
                distinguishing_hash: "debug_hash_info".to_string(),
                anonymous: false,
            },
            lazy: false,
            params: vec![Type::I32],
            results: vec![Type::F64],
        };

        let display = signature.display_signature();
        // Distinguishing hash should not appear in the signature
        assert_eq!(display, "fn my_function(I32) -> F64");
        assert!(!display.contains("debug_hash_info"));

        // Should still be able to parse it back
        let parsed: SymbolSignature = display.parse().unwrap();
        match parsed {
            SymbolSignature::Function {
                name,
                lazy,
                params,
                results,
            } => {
                assert_eq!(name.name, "my_function");
                // The parsed version won't have the distinguishing hash
                assert_eq!(name.distinguishing_hash, "");
                assert!(!lazy);
                assert_eq!(params, vec![Type::I32]);
                assert_eq!(results, vec![Type::F64]);
            }
            _ => panic!("Expected function signature"),
        }
    }
}
