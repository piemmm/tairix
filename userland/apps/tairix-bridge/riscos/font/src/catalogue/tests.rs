use super::*;

fn scanned(identifier: &str, element: usize) -> Font {
    Font {
        identifier: identifier.as_bytes().to_vec(),
        name: None,
        default: false,
        symbol: false,
        element,
    }
}

#[test]
fn a_messages_file_names_fonts_encodings_and_each_family_s_default() {
    let mut budget = 10;
    let holding = from_messages(
        b"# A library\nLFont_Omega:Big O*\nFont_Psi:\nLFont_Trinity.Medium:*\nEncoding_MyEnc2:\nBEncoding_Base9:\nnonsense\n",
        3,
        &mut budget,
    );
    assert_eq!(budget, 5, "each font and encoding listed, nothing else");
    assert_eq!(holding.fonts.len(), 3);
    assert_eq!(holding.fonts[0].shown(), b"Big O");
    assert!(holding.fonts[0].default && !holding.fonts[0].symbol);
    assert_eq!(holding.fonts[1].name, None);
    assert!(holding.fonts[1].symbol);
    assert_eq!(
        holding.fonts[2].name, None,
        "a lone * names it by its identifier"
    );
    assert!(holding.fonts[2].default);
    assert_eq!(holding.encodings.len(), 2);
    assert!(holding.encodings[1].base);
    assert!(holding.fonts.iter().all(|font| font.element == 3));
}

#[test]
fn a_messages_file_lists_no_more_than_the_catalogue_has_left() {
    let mut budget = 2;
    let holding = from_messages(
        b"LFont_A:\n# a comment\nEncoding_B:\nLFont_C:\n",
        0,
        &mut budget,
    );
    assert_eq!(
        (holding.fonts.len(), holding.encodings.len(), budget),
        (1, 1, 0)
    );
    let mut spent = 0;
    assert_eq!(
        from_messages(b"LFont_D:\n", 1, &mut spent),
        Holding::default()
    );
}

#[test]
fn the_first_element_to_hold_a_font_has_it_and_names_order_ignoring_case() {
    let holdings = alloc::vec![
        Holding {
            fonts: alloc::vec![scanned("zeta", 0), scanned("Alpha", 0), scanned("beta", 0)],
            encodings: Vec::new(),
        },
        Holding {
            fonts: alloc::vec![
                scanned("ALPHA", 1),
                Font {
                    name: Some(b"Big O".to_vec()),
                    ..scanned("Omega", 1)
                },
                scanned("Corpus.Bold", 1),
            ],
            encodings: Vec::new(),
        },
    ];
    let catalogue = Catalogue::of(Vec::new(), Vec::new(), holdings);
    let shown: Vec<&[u8]> = catalogue.fonts.iter().map(Font::shown).collect();
    assert_eq!(
        shown,
        [&b"Alpha"[..], b"beta", b"Big O", b"Corpus.Bold", b"zeta"]
    );
    assert_eq!(catalogue.font(b"alpha").map(|font| font.element), Some(0));
    assert_eq!(
        catalogue.font(b"OMEGA").map(Font::shown),
        Some(&b"Big O"[..])
    );
    assert_eq!(catalogue.font(b"Corpus"), None);
}

#[test]
fn fonts_shown_alike_keep_the_order_their_elements_hold_them_in() {
    let named = |identifier: &str, element: usize| Font {
        name: Some(b"Same".to_vec()),
        ..scanned(identifier, element)
    };
    let holdings = alloc::vec![
        Holding {
            fonts: alloc::vec![named("Zulu", 0), named("Mike", 0)],
            encodings: Vec::new(),
        },
        Holding {
            fonts: alloc::vec![named("alpha", 1), named("zulu", 1)],
            encodings: Vec::new(),
        },
    ];
    let catalogue = Catalogue::of(Vec::new(), Vec::new(), holdings);
    let identifiers: Vec<&[u8]> = catalogue
        .fonts
        .iter()
        .map(|font| font.identifier.as_slice())
        .collect();
    assert_eq!(identifiers, [&b"Zulu"[..], b"Mike", b"alpha"]);
    for identifier in [&b"zULU"[..], b"mike", b"ALPHA"] {
        assert!(catalogue
            .font(identifier)
            .is_some_and(|font| font.identifier.eq_ignore_ascii_case(identifier)));
    }
}

#[test]
fn a_base_encoding_is_found_by_its_identifier_and_a_listed_one_is_none() {
    let encoding = |identifier: &str, base: bool, element: usize| Encoding {
        identifier: identifier.as_bytes().to_vec(),
        name: None,
        base,
        element,
    };
    let holdings = alloc::vec![
        Holding {
            fonts: Vec::new(),
            encodings: alloc::vec![encoding("Latin1", false, 0), encoding("Base0", true, 0)],
        },
        Holding {
            fonts: Vec::new(),
            encodings: alloc::vec![encoding("LATIN1", true, 1), encoding("Base1", true, 1)],
        },
    ];
    let catalogue = Catalogue::of(Vec::new(), Vec::new(), holdings);
    assert_eq!(catalogue.base(b"base0").map(|e| e.element), Some(0));
    assert_eq!(catalogue.base(b"Base1").map(|e| e.element), Some(1));
    assert_eq!(
        catalogue.base(b"Latin1"),
        None,
        "the first to hold it lists it"
    );
    assert_eq!(catalogue.listed_encodings().count(), 1);
}

#[test]
fn families_gather_by_the_first_full_stop_and_choose_as_risc_os_does() {
    let names: [&[u8]; 6] = [
        b"Ab.Cd.Ef",
        b"Alpha",
        b"Alpha.Bold",
        b"Corpus.Bold",
        b"Corpus.Medium",
        b"Zed.Bold",
    ];
    let families = families(names.into_iter());
    assert_eq!(families.len(), 4);
    assert_eq!(families[0].members[0].label, Some(&b"Cd.Ef"[..]));
    assert!(
        families[0].opens(),
        "a family of one other member still opens"
    );
    assert!(families[1].opens());
    let defaults = |index: usize| index == 4;
    assert_eq!(families[1].chosen(defaults).map(|m| m.index), Some(1));
    assert_eq!(families[2].chosen(defaults).map(|m| m.index), Some(4));
    assert_eq!(families[3].chosen(defaults).map(|m| m.index), Some(5));
    let lone = super::families([&b"WIMPSymbol"[..]].into_iter());
    assert!(!lone[0].opens());
}

#[test]
fn a_family_gathers_its_members_whatever_sorts_between_them() {
    let names: [&[u8]; 4] = [b"corpus", b"Corpus-X", b"CORPUS.Bold", b"Corpus-X.Bold"];
    let families = families(names.into_iter());
    let gathered: Vec<(&[u8], Vec<usize>)> = families
        .iter()
        .map(|family| {
            (
                family.name,
                family.members.iter().map(|m| m.index).collect(),
            )
        })
        .collect();
    assert_eq!(
        gathered,
        [
            (&b"corpus"[..], alloc::vec![0, 2]),
            (b"Corpus-X", alloc::vec![1, 3])
        ],
        "named as its first member names it"
    );
}
