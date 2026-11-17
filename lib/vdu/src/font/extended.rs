//! The system font's characters past RISC OS's arrangement that
//! International's alphabets hold: Latin's other letters, Greek, Cyrillic
//! and Hebrew, drawn as the rest are. An accented letter is an accent's
//! two rows above a letter's lower six, or a letter with a mark in its
//! descender row, and a letter two scripts share is drawn once.

use super::{draw, Art, Glyph, ART, FIRST, SPACE};

/// A glyph's top two rows: an accent.
type Top = [&'static str; 2];
/// A glyph's lower six rows: a letter beneath an accent.
type Body = [&'static str; 6];

const ACUTE: Top = ["....##..", "...##..."];
const BREVE: Top = [".##..##.", "..####.."];
const CARON: Top = [".##..##.", "...##..."];
const CIRCUMFLEX: Top = ["...##...", ".##..##."];
const DIAERESIS: Top = [".##..##.", "........"];
const DIALYTIKA_TONOS: Top = ["....##..", "##.##.##"];
const DOT: Top = ["...##...", "........"];
const DOUBLE_ACUTE: Top = ["..##.##.", ".##.##.."];
const GRAVE: Top = ["..##....", "...##..."];
const MACRON: Top = [".######.", "........"];
const RING: Top = ["...##...", "..#..#.."];
const TILDE: Top = [".###.##.", "##.###.."];
const TURNED_COMMA: Top = ["...##...", "..##...."];

const CEDILLA: &str = "...##...";
const COMMA: &str = "..##....";
const OGONEK: &str = ".....##.";

const BLANK: Body = body(SPACE);
const CAP_A: Body = body(of(192));
const CAP_E: Body = body(of(200));
const CAP_I: Body = body(of(204));
const CAP_N: Body = body(of(209));
const CAP_O: Body = body(of(210));
const CAP_U: Body = body(of(217));
const CAP_W: Body = body(of(129));
const CAP_Y: Body = body(of(221));
#[rustfmt::skip]
const CAP_B: Body = [".#####..", ".##..##.", ".#####..", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const CAP_C: Body = ["..####..", ".##..##.", ".##.....", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const CAP_CYRILLIC_I: Body = ["##...##.", "##..###.", "##.####.", "####.##.", "###..##.", "........"];
#[rustfmt::skip]
const CAP_CYRILLIC_U: Body = [".##..##.", ".##..##.", "..#####.", ".....##.", "..####..", "........"];
#[rustfmt::skip]
const CAP_D: Body = [".####...", ".##.##..", ".##..##.", ".##.##..", ".####...", "........"];
#[rustfmt::skip]
const CAP_F: Body = [".######.", ".##.....", ".#####..", ".##.....", ".##.....", "........"];
#[rustfmt::skip]
const CAP_G: Body = ["..####..", ".##.....", ".##.###.", ".##..##.", "..#####.", "........"];
#[rustfmt::skip]
const CAP_GAMMA: Body = [".#######", ".##....#", ".##.....", ".##.....", ".##.....", "........"];
#[rustfmt::skip]
const CAP_H: Body = [".##..##.", ".##..##.", ".######.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const CAP_J: Body = ["...#####", ".....##.", ".....##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const CAP_K: Body = [".##..##.", ".##.##..", ".####...", ".##.##..", ".##..###", "........"];
#[rustfmt::skip]
const CAP_L: Body = [".##.....", ".##.....", ".##.....", ".##.....", ".######.", "........"];
#[rustfmt::skip]
const CAP_M: Body = ["##...##.", "###.###.", "##.#.##.", "##...##.", "##...##.", "........"];
#[rustfmt::skip]
const CAP_P: Body = [".#####..", ".##..##.", ".#####..", ".##.....", ".##.....", "........"];
#[rustfmt::skip]
const CAP_R: Body = [".#####..", ".##..##.", ".#####..", ".##.##..", ".##..##.", "........"];
#[rustfmt::skip]
const CAP_S: Body = ["..####..", ".##.....", "..####..", ".....##.", "..####..", "........"];
#[rustfmt::skip]
const CAP_T: Body = [".######.", "...##...", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const CAP_Z: Body = [".######.", ".....##.", "...##...", ".##.....", ".######.", "........"];

#[rustfmt::skip]
const ALEF: Art = ["........", ".##...#.", "..##..#.", "...####.", "..#.##..", ".#...##.", ".#....#.", "........"];
#[rustfmt::skip]
const ALPHA: Art = ["........", "........", "..###.#.", ".##.###.", ".##..##.", ".##.###.", "..###.##", "........"];
#[rustfmt::skip]
const ALPHA_TONOS: Art = ["#...##..", "#..####.", "..##..##", "..##..##", "..######", "..##..##", "..##..##", "........"];
#[rustfmt::skip]
const AYIN: Art = ["........", ".##..##.", ".##..##.", "..#..##.", "...####.", "...##...", "#####...", "........"];
#[rustfmt::skip]
const BE: Art = [".######.", ".##.....", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const BET: Art = ["........", ".#####..", ".....##.", ".....##.", ".....##.", ".....##.", "#######.", "........"];
#[rustfmt::skip]
const BETA: Art = ["..####..", ".##..##.", ".#####..", ".##..##.", ".##..##.", ".#####..", ".##.....", ".##....."];
#[rustfmt::skip]
const BE_SMALL: Art = ["....###.", "..##....", ".#####..", ".##..##.", ".##..##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const B_DOT: Art = [".##.##..", ".##.....", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const CAP_ENG: Art = [".##..##.", ".###.##.", ".######.", ".##.###.", ".##..##.", ".##..##.", ".##..##.", "....##.."];
#[rustfmt::skip]
const CAP_H_STROKE: Art = [".##..##.", "########", ".##..##.", ".######.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const CAP_L_CARON: Art = [".##..#..", ".##..#..", ".##.....", ".##.....", ".##.....", ".##.....", ".######.", "........"];
#[rustfmt::skip]
const CAP_L_STROKE: Art = [".##.....", ".##.....", ".##.#...", ".###....", "###.....", ".##.....", ".######.", "........"];
#[rustfmt::skip]
const CAP_T_STROKE: Art = [".######.", "...##...", "...##...", "..####..", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const CHE: Art = [".##..##.", ".##..##.", ".##..##.", "..#####.", ".....##.", ".....##.", ".....##.", "........"];
#[rustfmt::skip]
const CHE_SMALL: Art = ["........", "........", ".##..##.", ".##..##.", "..#####.", ".....##.", ".....##.", "........"];
#[rustfmt::skip]
const CHI: Art = ["........", "........", ".##..##.", "..####..", "...##...", "..####..", ".##..##.", ".##..##."];
#[rustfmt::skip]
const CYRILLIC_E: Art = ["..####..", ".##..##.", ".....##.", "..#####.", ".....##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const CYRILLIC_I: Art = [".##..##.", ".##..##.", ".##.###.", ".######.", ".###.##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const CYRILLIC_U: Art = [".##..##.", ".##..##.", ".##..##.", "..#####.", ".....##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const DALET: Art = ["........", "#######.", ".....##.", ".....##.", ".....##.", ".....##.", ".....##.", "........"];
#[rustfmt::skip]
const DE: Art = ["..#####.", "..##.##.", "..##.##.", ".##..##.", ".##..##.", "########", "##....##", "........"];
#[rustfmt::skip]
const DELTA: Art = ["...##...", "..####..", "..#..#..", ".##..##.", ".#....#.", "##....##", "########", "........"];
#[rustfmt::skip]
const DELTA_SMALL: Art = ["..####..", ".##.....", "..###...", ".##.##..", ".##..##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const DE_SMALL: Art = ["........", "........", "..####..", ".##.##..", ".##.##..", "#######.", "##...##.", "........"];
#[rustfmt::skip]
const DJE: Art = ["######..", "..##....", "..#####.", "..##..##", "..##..##", "..##..##", "..##.##.", "........"];
#[rustfmt::skip]
const DJE_SMALL: Art = ["##......", "#####...", ".##.....", ".#####..", ".##..##.", ".##..##.", ".##..##.", "....##.."];
#[rustfmt::skip]
const DOTLESS_I: Art = ["........", "........", "..###...", "...##...", "...##...", "...##...", "..####..", "........"];
#[rustfmt::skip]
const DOUBLE_LOW_LINE: Art = ["........", "........", "........", "........", "........", "########", "........", "########"];
#[rustfmt::skip]
const DRACHMA: Art = ["........", "...#....", "..#.#...", ".#...#..", "#######.", "....#.#.", "....##..", "....#..."];
#[rustfmt::skip]
const DZHE: Art = [".##..##.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".######.", "...##...", "........"];
#[rustfmt::skip]
const DZHE_SMALL: Art = ["........", "........", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".######.", "...##..."];
#[rustfmt::skip]
const D_CARON: Art = ["....##.#", "....##.#", ".#####..", "##..##..", "##..##..", "##..##..", ".#####..", "........"];
#[rustfmt::skip]
const D_DOT: Art = ["..##.##.", ".....##.", "..#####.", ".##..##.", ".##..##.", ".##..##.", "..#####.", "........"];
#[rustfmt::skip]
const D_STROKE: Art = [".....##.", "....####", "..#####.", ".##..##.", ".##..##.", ".##..##.", "..#####.", "........"];
#[rustfmt::skip]
const EF_SMALL: Art = ["...##...", "...##...", ".######.", "##.##.##", "##.##.##", ".######.", "...##...", "...##..."];
#[rustfmt::skip]
const EL: Art = ["...####.", "..##.##.", "..##.##.", "..##.##.", "..##.##.", ".##..##.", "##...##.", "........"];
#[rustfmt::skip]
const EL_SMALL: Art = ["........", "........", "...####.", "..##.##.", "..##.##.", ".##..##.", "##...##.", "........"];
#[rustfmt::skip]
const EM_SMALL: Art = ["........", "........", "##...##.", "###.###.", "##.#.##.", "##...##.", "##...##.", "........"];
#[rustfmt::skip]
const ENG: Art = ["........", "........", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "....##.."];
#[rustfmt::skip]
const EN_SMALL: Art = ["........", "........", ".##..##.", ".##..##.", ".######.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const EPSILON: Art = ["........", "........", "..####..", ".##.....", "..###...", ".##.....", "..####..", "........"];
#[rustfmt::skip]
const EPSILON_TONOS: Art = ["#.######", "#.##....", "..##....", "..#####.", "..##....", "..##....", "..######", "........"];
#[rustfmt::skip]
const ETA: Art = ["........", "........", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".....##."];
#[rustfmt::skip]
const ETA_TONOS: Art = ["#.##..##", "#.##..##", "..##..##", "..######", "..##..##", "..##..##", "..##..##", "........"];
#[rustfmt::skip]
const E_SMALL: Art = ["........", "........", ".#####..", ".....##.", "..#####.", ".....##.", ".#####..", "........"];
#[rustfmt::skip]
const FINAL_KAF: Art = ["........", "######..", ".....##.", ".....##.", ".....##.", ".....##.", ".....##.", ".....##."];
#[rustfmt::skip]
const FINAL_MEM: Art = ["........", "######..", "##...##.", "##...##.", "##...##.", "##...##.", "#######.", "........"];
#[rustfmt::skip]
const FINAL_NUN: Art = ["........", "..###...", "...##...", "...##...", "...##...", "...##...", "...##...", "...##..."];
#[rustfmt::skip]
const FINAL_PE: Art = ["........", "######..", "##...##.", "###..##.", ".....##.", ".....##.", ".....##.", ".....##."];
#[rustfmt::skip]
const FINAL_SIGMA: Art = ["........", "........", "..####..", ".##.....", ".##.....", "..####..", ".....##.", "....##.."];
#[rustfmt::skip]
const FINAL_TSADI: Art = ["........", "##..##..", ".##.##..", "..###...", "..##....", "..##....", "..##....", "..##...."];
#[rustfmt::skip]
const F_DOT: Art = ["...###..", "..##..##", "..##....", ".#####..", "..##....", "..##....", "..##....", "........"];
#[rustfmt::skip]
const GAMMA: Art = [".######.", ".##.....", ".##.....", ".##.....", ".##.....", ".##.....", ".##.....", "........"];
#[rustfmt::skip]
const GAMMA_SMALL: Art = ["........", "........", ".##..##.", "..#..#..", "..####..", "...##...", "...##...", "...##..."];
#[rustfmt::skip]
const GHE_SMALL: Art = ["........", "........", ".######.", ".##.....", ".##.....", ".##.....", ".##.....", "........"];
#[rustfmt::skip]
const GIMEL: Art = ["........", "..###...", "....##..", "....##..", "...###..", "..##.##.", ".##..##.", "........"];
#[rustfmt::skip]
const HARD_SIGN: Art = ["###.....", ".##.....", ".##.....", ".#####..", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const HARD_SIGN_SMALL: Art = ["........", "........", "###.....", ".#####..", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const HE: Art = ["........", "#######.", ".....##.", "##...##.", "##...##.", "##...##.", "##...##.", "........"];
#[rustfmt::skip]
const HET: Art = ["........", "#######.", "##...##.", "##...##.", "##...##.", "##...##.", "##...##.", "........"];
#[rustfmt::skip]
const H_CIRCUMFLEX: Art = [".##..#..", ".##.#.#.", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const H_STROKE: Art = [".##.....", "####....", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const IOTA: Art = ["........", "........", "..##....", "..##....", "..##....", "..##.##.", "...###..", "........"];
#[rustfmt::skip]
const IOTA_TONOS: Art = ["#..####.", "#...##..", "....##..", "....##..", "....##..", "....##..", "...####.", "........"];
#[rustfmt::skip]
const I_SMALL: Art = ["........", "........", ".##..##.", ".##.###.", ".######.", ".###.##.", ".##..##.", "........"];
#[rustfmt::skip]
const J_CIRCUMFLEX: Art = ["....##..", "..##..##", "...###..", "....##..", "....##..", "....##..", "....##..", ".####..."];
#[rustfmt::skip]
const KAF: Art = ["........", "######..", ".....##.", ".....##.", ".....##.", ".....##.", "######..", "........"];
#[rustfmt::skip]
const KRA: Art = ["........", "........", ".##..##.", ".##.##..", ".####...", ".##.##..", ".##..##.", "........"];
#[rustfmt::skip]
const LAMDA: Art = ["...##...", "..####..", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const LAMDA_SMALL: Art = [".##.....", "..##....", "...##...", "..####..", ".##..##.", ".##..##.", "##....##", "........"];
#[rustfmt::skip]
const LAMED: Art = ["##......", "##......", "#######.", ".....##.", ".....##.", "....##..", "...##...", "........"];
#[rustfmt::skip]
const LJE: Art = ["..##....", ".#.#....", ".#.####.", ".#.#..##", ".#.#..##", ".#.#..##", "##.####.", "........"];
#[rustfmt::skip]
const LJE_SMALL: Art = ["........", "........", "..##....", ".#.#....", ".#.####.", ".#.#..##", "##.####.", "........"];
#[rustfmt::skip]
const L_ACUTE: Art = [".##..##.", ".##.##..", ".##.....", ".##.....", ".##.....", ".##.....", "..###...", "........"];
#[rustfmt::skip]
const L_CARON: Art = ["..###.#.", "...##.#.", "...##...", "...##...", "...##...", "...##...", "..####..", "........"];
#[rustfmt::skip]
const L_STROKE: Art = ["..###...", "...##...", "...##.#.", "..####..", ".#.##...", "...##...", "..####..", "........"];
#[rustfmt::skip]
const MEM: Art = ["........", "##.##...", ".#####..", ".##..##.", ".##..##.", ".##..##.", ".##.###.", "........"];
#[rustfmt::skip]
const NJE: Art = ["##.#....", "##.#....", "##.####.", "####..##", "##.#..##", "##.#..##", "##.####.", "........"];
#[rustfmt::skip]
const NJE_SMALL: Art = ["........", "........", "##.#....", "##.#....", "#######.", "##.#..##", "##.####.", "........"];
#[rustfmt::skip]
const NUMERO: Art = ["#..#....", "##.#.###", "#.##.#.#", "#..#.###", "#..#....", "#..#.###", "#..#....", "........"];
#[rustfmt::skip]
const NUN: Art = ["........", "..###...", "...##...", "...##...", "...##...", "...##...", ".#####..", "........"];
#[rustfmt::skip]
const OGONEK_SPACING: Art = ["........", "........", "........", "........", "........", "........", "...##...", "....###."];
#[rustfmt::skip]
const OMEGA: Art = ["..####..", ".##..##.", ".##..##.", ".##..##.", "..#..#..", "..#..#..", "###..###", "........"];
#[rustfmt::skip]
const OMEGA_SMALL: Art = ["........", "........", ".##..##.", "##....##", "##.##.##", "##.##.##", ".##..##.", "........"];
#[rustfmt::skip]
const OMEGA_TONOS: Art = ["#..####.", "#.##..##", "..##..##", "..##..##", "...#..#.", "...#..#.", "..##..##", "........"];
#[rustfmt::skip]
const OMICRON_TONOS: Art = ["#..####.", "#.##..##", "..##..##", "..##..##", "..##..##", "..##..##", "...####.", "........"];
#[rustfmt::skip]
const OVERLINE: Art = ["########", "........", "........", "........", "........", "........", "........", "........"];
#[rustfmt::skip]
const PE: Art = ["........", "######..", "##...##.", "###..##.", ".....##.", ".....##.", "#######.", "........"];
#[rustfmt::skip]
const PE_SMALL: Art = ["........", "........", ".######.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const PHI: Art = ["...##...", ".######.", "##.##.##", "##.##.##", "##.##.##", ".######.", "...##...", "........"];
#[rustfmt::skip]
const PHI_SMALL: Art = ["........", "...##...", ".######.", "##.##.##", "##.##.##", ".######.", "...##...", "...##..."];
#[rustfmt::skip]
const PI: Art = [".######.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const PI_SMALL: Art = ["........", "........", "#######.", ".##.##..", ".##.##..", ".##.##..", ".##..##.", "........"];
#[rustfmt::skip]
const PSI: Art = ["##.##.##", "##.##.##", "##.##.##", ".######.", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const PSI_SMALL: Art = ["........", "........", "##.##.##", "##.##.##", "##.##.##", ".######.", "...##...", "...##..."];
#[rustfmt::skip]
const QOF: Art = ["........", "#######.", ".....##.", "##..##..", "##.##...", "##......", "##......", "##......"];
#[rustfmt::skip]
const RESH: Art = ["........", "######..", ".....##.", ".....##.", ".....##.", ".....##.", ".....##.", "........"];
#[rustfmt::skip]
const RHO: Art = ["........", "........", "..####..", ".##..##.", ".##..##.", ".#####..", ".##.....", ".##....."];
#[rustfmt::skip]
const SAMEKH: Art = ["........", "######..", "##...##.", "##...##.", "##...##.", "##..##..", ".####...", "........"];
#[rustfmt::skip]
const SHA: Art = ["##.##.##", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "########", "........"];
#[rustfmt::skip]
const SHA_SMALL: Art = ["........", "........", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "########", "........"];
#[rustfmt::skip]
const SHCHA: Art = ["##.##.##", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "########", "......##", "........"];
#[rustfmt::skip]
const SHCHA_SMALL: Art = ["........", "........", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "########", "......##"];
#[rustfmt::skip]
const SHIN: Art = ["........", "##.##.##", "##.##.##", "##.##.##", "##.##.##", "##.##.#.", "#######.", "........"];
#[rustfmt::skip]
const SIGMA: Art = [".######.", ".##.....", "..##....", "...##...", "..##....", ".##.....", ".######.", "........"];
#[rustfmt::skip]
const SIGMA_SMALL: Art = ["........", "........", "..######", ".##.##..", ".##..##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const SOFT_SIGN: Art = [".##.....", ".##.....", ".##.....", ".#####..", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const SOFT_SIGN_SMALL: Art = ["........", "........", ".##.....", ".#####..", ".##..##.", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const TAU: Art = ["........", "........", ".######.", "...##...", "...##...", "...##.#.", "....##..", "........"];
#[rustfmt::skip]
const TAV: Art = ["........", "#######.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "###..##.", "........"];
#[rustfmt::skip]
const TET: Art = ["........", "##.###..", "##.#.##.", "##...##.", "##...##.", "##...##.", ".#####..", "........"];
#[rustfmt::skip]
const TE_SMALL: Art = ["........", "........", ".######.", "...##...", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const THETA: Art = ["..####..", ".##..##.", ".##..##.", ".######.", ".##..##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const THETA_SMALL: Art = ["...##...", "..#..#..", ".##..##.", ".######.", ".##..##.", "..#..#..", "...##...", "........"];
#[rustfmt::skip]
const TSADI: Art = ["........", "##..##..", ".##.##..", "..###...", "...##...", "....##..", "#######.", "........"];
#[rustfmt::skip]
const TSE: Art = [".##..##.", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".#######", "......##", "........"];
#[rustfmt::skip]
const TSE_SMALL: Art = ["........", "........", ".##..##.", ".##..##.", ".##..##.", ".##..##.", ".#######", "......##"];
#[rustfmt::skip]
const TSHE: Art = ["######..", "..##....", "..#####.", "..##..##", "..##..##", "..##..##", "..##..##", "........"];
#[rustfmt::skip]
const TSHE_SMALL: Art = ["##......", "#####...", ".##.....", ".#####..", ".##..##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const T_CARON: Art = ["..##.#..", "..##.#..", ".#####..", "..##....", "..##....", "..##.##.", "...###..", "........"];
#[rustfmt::skip]
const T_DOT: Art = ["..##.##.", "..##....", ".#####..", "..##....", "..##....", "..##.##.", "...###..", "........"];
#[rustfmt::skip]
const T_STROKE: Art = ["..##....", "..##....", ".#####..", "..##....", ".#####..", "..##.##.", "...###..", "........"];
#[rustfmt::skip]
const UKRAINIAN_IE: Art = ["..####..", ".##..##.", ".##.....", ".#####..", ".##.....", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const UKRAINIAN_IE_SMALL: Art = ["........", "........", "..####..", ".##.....", ".#####..", ".##.....", "..####..", "........"];
#[rustfmt::skip]
const UPSILON: Art = ["........", "........", ".##..##.", ".##..##.", ".##..##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const UPSILON_TONOS: Art = ["#.##..##", "#.##..##", "..##..##", "...####.", "....##..", "....##..", "....##..", "........"];
#[rustfmt::skip]
const VAV: Art = ["........", "..###...", "...##...", "...##...", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const VE_SMALL: Art = ["........", "........", ".#####..", ".##..##.", ".#####..", ".##..##.", ".#####..", "........"];
#[rustfmt::skip]
const XI: Art = [".######.", "........", "........", "..####..", "........", "........", ".######.", "........"];
#[rustfmt::skip]
const XI_SMALL: Art = [".######.", "..##....", "...###..", "..##....", ".##.....", "..####..", ".....##.", "....##.."];
#[rustfmt::skip]
const YA: Art = ["..#####.", ".##..##.", ".##..##.", "..#####.", "..##.##.", ".##..##.", ".##..##.", "........"];
#[rustfmt::skip]
const YA_SMALL: Art = ["........", "........", "..#####.", ".##..##.", "..#####.", "..##.##.", ".##..##.", "........"];
#[rustfmt::skip]
const YERU: Art = ["##....##", "##....##", "##....##", "#####.##", "##..####", "##..####", "#####.##", "........"];
#[rustfmt::skip]
const YERU_SMALL: Art = ["........", "........", "##....##", "#####.##", "##..####", "##..####", "#####.##", "........"];
#[rustfmt::skip]
const YOD: Art = ["........", "..###...", "...##...", "...##...", "........", "........", "........", "........"];
#[rustfmt::skip]
const YPOGEGRAMMENI: Art = ["........", "........", "........", "........", "........", "........", "...##...", "....#..."];
#[rustfmt::skip]
const YU: Art = ["#..####.", "#.##..##", "#.##..##", "####..##", "#.##..##", "#.##..##", "#..####.", "........"];
#[rustfmt::skip]
const YU_SMALL: Art = ["........", "........", "#..####.", "#.##..##", "####..##", "#.##..##", "#..####.", "........"];
#[rustfmt::skip]
const ZAYIN: Art = ["........", ".#####..", "...##...", "...##...", "...##...", "...##...", "...##...", "........"];
#[rustfmt::skip]
const ZE: Art = ["..####..", ".##..##.", ".....##.", "...###..", ".....##.", ".##..##.", "..####..", "........"];
#[rustfmt::skip]
const ZETA: Art = [".######.", "...##...", "..##....", ".##.....", ".##.....", "..####..", ".....##.", "....##.."];
#[rustfmt::skip]
const ZE_SMALL: Art = ["........", "........", ".#####..", ".....##.", "...###..", ".....##.", ".#####..", "........"];
#[rustfmt::skip]
const ZHE: Art = ["##.##.##", "##.##.##", ".######.", "..####..", ".######.", "##.##.##", "##.##.##", "........"];
#[rustfmt::skip]
const ZHE_SMALL: Art = ["........", "........", "##.##.##", ".######.", "..####..", ".######.", "##.##.##", "........"];

/// The glyph RISC OS's arrangement draws at `code`.
const fn of(code: u8) -> Art {
    ART[(code - FIRST) as usize]
}

/// The lower six rows of `art`.
const fn body(art: Art) -> Body {
    [art[2], art[3], art[4], art[5], art[6], art[7]]
}

/// A small letter's lower six rows: all but its ascender or its dot.
const fn small(code: u8) -> Body {
    body(of(code))
}

/// `top` over `body`.
const fn above(top: Top, body: Body) -> Art {
    [
        top[0], top[1], body[0], body[1], body[2], body[3], body[4], body[5],
    ]
}

/// `art` with `mark` in its descender row.
const fn below(art: Art, mark: &'static str) -> Art {
    [art[0], art[1], art[2], art[3], art[4], art[5], art[6], mark]
}

/// Each character and its drawing, in order.
#[rustfmt::skip]
static DRAWINGS: [(char, Art); 341] = [
    ('Ā', above(MACRON, CAP_A)),
    ('ā', above(MACRON, small(b'a'))),
    ('Ă', above(BREVE, CAP_A)),
    ('ă', above(BREVE, small(b'a'))),
    ('Ą', below(of(b'A'), OGONEK)),
    ('ą', below(of(b'a'), OGONEK)),
    ('Ć', above(ACUTE, CAP_C)),
    ('ć', above(ACUTE, small(b'c'))),
    ('Ĉ', above(CIRCUMFLEX, CAP_C)),
    ('ĉ', above(CIRCUMFLEX, small(b'c'))),
    ('Ċ', above(DOT, CAP_C)),
    ('ċ', above(DOT, small(b'c'))),
    ('Č', above(CARON, CAP_C)),
    ('č', above(CARON, small(b'c'))),
    ('Ď', above(CARON, CAP_D)),
    ('ď', D_CARON),
    ('Đ', of(208)),
    ('đ', D_STROKE),
    ('Ē', above(MACRON, CAP_E)),
    ('ē', above(MACRON, small(b'e'))),
    ('Ė', above(DOT, CAP_E)),
    ('ė', above(DOT, small(b'e'))),
    ('Ę', below(of(b'E'), OGONEK)),
    ('ę', below(of(b'e'), OGONEK)),
    ('Ě', above(CARON, CAP_E)),
    ('ě', above(CARON, small(b'e'))),
    ('Ĝ', above(CIRCUMFLEX, CAP_G)),
    ('ĝ', above(CIRCUMFLEX, small(b'g'))),
    ('Ğ', above(BREVE, CAP_G)),
    ('ğ', above(BREVE, small(b'g'))),
    ('Ġ', above(DOT, CAP_G)),
    ('ġ', above(DOT, small(b'g'))),
    ('Ģ', below(of(b'G'), COMMA)),
    ('ģ', above(TURNED_COMMA, small(b'g'))),
    ('Ĥ', above(CIRCUMFLEX, CAP_H)),
    ('ĥ', H_CIRCUMFLEX),
    ('Ħ', CAP_H_STROKE),
    ('ħ', H_STROKE),
    ('Ĩ', above(TILDE, CAP_I)),
    ('ĩ', above(TILDE, small(b'i'))),
    ('Ī', above(MACRON, CAP_I)),
    ('ī', above(MACRON, small(b'i'))),
    ('Į', below(of(b'I'), OGONEK)),
    ('į', below(of(b'i'), OGONEK)),
    ('İ', above(DOT, CAP_I)),
    ('ı', DOTLESS_I),
    ('Ĵ', above(CIRCUMFLEX, CAP_J)),
    ('ĵ', J_CIRCUMFLEX),
    ('Ķ', below(of(b'K'), COMMA)),
    ('ķ', below(of(b'k'), COMMA)),
    ('ĸ', KRA),
    ('Ĺ', above(ACUTE, CAP_L)),
    ('ĺ', L_ACUTE),
    ('Ļ', below(of(b'L'), COMMA)),
    ('ļ', below(of(b'l'), COMMA)),
    ('Ľ', CAP_L_CARON),
    ('ľ', L_CARON),
    ('Ł', CAP_L_STROKE),
    ('ł', L_STROKE),
    ('Ń', above(ACUTE, CAP_N)),
    ('ń', above(ACUTE, small(b'n'))),
    ('Ņ', below(of(b'N'), COMMA)),
    ('ņ', below(of(b'n'), COMMA)),
    ('Ň', above(CARON, CAP_N)),
    ('ň', above(CARON, small(b'n'))),
    ('Ŋ', CAP_ENG),
    ('ŋ', ENG),
    ('Ō', above(MACRON, CAP_O)),
    ('ō', above(MACRON, small(b'o'))),
    ('Ő', above(DOUBLE_ACUTE, CAP_O)),
    ('ő', above(DOUBLE_ACUTE, small(b'o'))),
    ('Ŕ', above(ACUTE, CAP_R)),
    ('ŕ', above(ACUTE, small(b'r'))),
    ('Ŗ', below(of(b'R'), COMMA)),
    ('ŗ', below(of(b'r'), COMMA)),
    ('Ř', above(CARON, CAP_R)),
    ('ř', above(CARON, small(b'r'))),
    ('Ś', above(ACUTE, CAP_S)),
    ('ś', above(ACUTE, small(b's'))),
    ('Ŝ', above(CIRCUMFLEX, CAP_S)),
    ('ŝ', above(CIRCUMFLEX, small(b's'))),
    ('Ş', below(of(b'S'), CEDILLA)),
    ('ş', below(of(b's'), CEDILLA)),
    ('Š', above(CARON, CAP_S)),
    ('š', above(CARON, small(b's'))),
    ('Ţ', below(of(b'T'), CEDILLA)),
    ('ţ', below(of(b't'), CEDILLA)),
    ('Ť', above(CARON, CAP_T)),
    ('ť', T_CARON),
    ('Ŧ', CAP_T_STROKE),
    ('ŧ', T_STROKE),
    ('Ũ', above(TILDE, CAP_U)),
    ('ũ', above(TILDE, small(b'u'))),
    ('Ū', above(MACRON, CAP_U)),
    ('ū', above(MACRON, small(b'u'))),
    ('Ŭ', above(BREVE, CAP_U)),
    ('ŭ', above(BREVE, small(b'u'))),
    ('Ů', above(RING, CAP_U)),
    ('ů', above(RING, small(b'u'))),
    ('Ű', above(DOUBLE_ACUTE, CAP_U)),
    ('ű', above(DOUBLE_ACUTE, small(b'u'))),
    ('Ų', below(of(b'U'), OGONEK)),
    ('ų', below(of(b'u'), OGONEK)),
    ('Ÿ', above(DIAERESIS, CAP_Y)),
    ('Ź', above(ACUTE, CAP_Z)),
    ('ź', above(ACUTE, small(b'z'))),
    ('Ż', above(DOT, CAP_Z)),
    ('ż', above(DOT, small(b'z'))),
    ('Ž', above(CARON, CAP_Z)),
    ('ž', above(CARON, small(b'z'))),
    ('Ș', below(of(b'S'), COMMA)),
    ('ș', below(of(b's'), COMMA)),
    ('Ț', below(of(b'T'), COMMA)),
    ('ț', below(of(b't'), COMMA)),
    ('ˇ', above(CARON, BLANK)),
    ('˘', above(BREVE, BLANK)),
    ('˙', above(DOT, BLANK)),
    ('˛', OGONEK_SPACING),
    ('˝', above(DOUBLE_ACUTE, BLANK)),
    ('ͺ', YPOGEGRAMMENI),
    ('\u{37E}', of(b';')),
    ('΄', of(180)),
    ('΅', above(DIALYTIKA_TONOS, BLANK)),
    ('Ά', ALPHA_TONOS),
    ('\u{387}', of(183)),
    ('Έ', EPSILON_TONOS),
    ('Ή', ETA_TONOS),
    ('Ί', IOTA_TONOS),
    ('Ό', OMICRON_TONOS),
    ('Ύ', UPSILON_TONOS),
    ('Ώ', OMEGA_TONOS),
    ('ΐ', above(DIALYTIKA_TONOS, body(IOTA))),
    ('Α', of(b'A')),
    ('Β', of(b'B')),
    ('Γ', GAMMA),
    ('Δ', DELTA),
    ('Ε', of(b'E')),
    ('Ζ', of(b'Z')),
    ('Η', of(b'H')),
    ('Θ', THETA),
    ('Ι', of(b'I')),
    ('Κ', of(b'K')),
    ('Λ', LAMDA),
    ('Μ', of(b'M')),
    ('Ν', of(b'N')),
    ('Ξ', XI),
    ('Ο', of(b'O')),
    ('Π', PI),
    ('Ρ', of(b'P')),
    ('Σ', SIGMA),
    ('Τ', of(b'T')),
    ('Υ', of(b'Y')),
    ('Φ', PHI),
    ('Χ', of(b'X')),
    ('Ψ', PSI),
    ('Ω', OMEGA),
    ('Ϊ', above(DIAERESIS, CAP_I)),
    ('Ϋ', above(DIAERESIS, CAP_Y)),
    ('ά', above(ACUTE, body(ALPHA))),
    ('έ', above(ACUTE, body(EPSILON))),
    ('ή', above(ACUTE, body(ETA))),
    ('ί', above(ACUTE, body(IOTA))),
    ('ΰ', above(DIALYTIKA_TONOS, body(UPSILON))),
    ('α', ALPHA),
    ('β', BETA),
    ('γ', GAMMA_SMALL),
    ('δ', DELTA_SMALL),
    ('ε', EPSILON),
    ('ζ', ZETA),
    ('η', ETA),
    ('θ', THETA_SMALL),
    ('ι', IOTA),
    ('κ', KRA),
    ('λ', LAMDA_SMALL),
    ('μ', of(181)),
    ('ν', of(b'v')),
    ('ξ', XI_SMALL),
    ('ο', of(b'o')),
    ('π', PI_SMALL),
    ('ρ', RHO),
    ('ς', FINAL_SIGMA),
    ('σ', SIGMA_SMALL),
    ('τ', TAU),
    ('υ', UPSILON),
    ('φ', PHI_SMALL),
    ('χ', CHI),
    ('ψ', PSI_SMALL),
    ('ω', OMEGA_SMALL),
    ('ϊ', above(DIAERESIS, body(IOTA))),
    ('ϋ', above(DIAERESIS, body(UPSILON))),
    ('ό', above(ACUTE, small(b'o'))),
    ('ύ', above(ACUTE, body(UPSILON))),
    ('ώ', above(ACUTE, body(OMEGA_SMALL))),
    ('Ё', above(DIAERESIS, CAP_E)),
    ('Ђ', DJE),
    ('Ѓ', above(ACUTE, CAP_GAMMA)),
    ('Є', UKRAINIAN_IE),
    ('Ѕ', of(b'S')),
    ('І', of(b'I')),
    ('Ї', above(DIAERESIS, CAP_I)),
    ('Ј', of(b'J')),
    ('Љ', LJE),
    ('Њ', NJE),
    ('Ћ', TSHE),
    ('Ќ', above(ACUTE, CAP_K)),
    ('Ў', above(BREVE, CAP_CYRILLIC_U)),
    ('Џ', DZHE),
    ('А', of(b'A')),
    ('Б', BE),
    ('В', of(b'B')),
    ('Г', GAMMA),
    ('Д', DE),
    ('Е', of(b'E')),
    ('Ж', ZHE),
    ('З', ZE),
    ('И', CYRILLIC_I),
    ('Й', above(BREVE, CAP_CYRILLIC_I)),
    ('К', of(b'K')),
    ('Л', EL),
    ('М', of(b'M')),
    ('Н', of(b'H')),
    ('О', of(b'O')),
    ('П', PI),
    ('Р', of(b'P')),
    ('С', of(b'C')),
    ('Т', of(b'T')),
    ('У', CYRILLIC_U),
    ('Ф', PHI),
    ('Х', of(b'X')),
    ('Ц', TSE),
    ('Ч', CHE),
    ('Ш', SHA),
    ('Щ', SHCHA),
    ('Ъ', HARD_SIGN),
    ('Ы', YERU),
    ('Ь', SOFT_SIGN),
    ('Э', CYRILLIC_E),
    ('Ю', YU),
    ('Я', YA),
    ('а', of(b'a')),
    ('б', BE_SMALL),
    ('в', VE_SMALL),
    ('г', GHE_SMALL),
    ('д', DE_SMALL),
    ('е', of(b'e')),
    ('ж', ZHE_SMALL),
    ('з', ZE_SMALL),
    ('и', I_SMALL),
    ('й', above(BREVE, body(I_SMALL))),
    ('к', KRA),
    ('л', EL_SMALL),
    ('м', EM_SMALL),
    ('н', EN_SMALL),
    ('о', of(b'o')),
    ('п', PE_SMALL),
    ('р', of(b'p')),
    ('с', of(b'c')),
    ('т', TE_SMALL),
    ('у', of(b'y')),
    ('ф', EF_SMALL),
    ('х', of(b'x')),
    ('ц', TSE_SMALL),
    ('ч', CHE_SMALL),
    ('ш', SHA_SMALL),
    ('щ', SHCHA_SMALL),
    ('ъ', HARD_SIGN_SMALL),
    ('ы', YERU_SMALL),
    ('ь', SOFT_SIGN_SMALL),
    ('э', E_SMALL),
    ('ю', YU_SMALL),
    ('я', YA_SMALL),
    ('ё', above(DIAERESIS, small(b'e'))),
    ('ђ', DJE_SMALL),
    ('ѓ', above(ACUTE, body(GHE_SMALL))),
    ('є', UKRAINIAN_IE_SMALL),
    ('ѕ', of(b's')),
    ('і', of(b'i')),
    ('ї', above(DIAERESIS, small(b'i'))),
    ('ј', of(b'j')),
    ('љ', LJE_SMALL),
    ('њ', NJE_SMALL),
    ('ћ', TSHE_SMALL),
    ('ќ', above(ACUTE, body(KRA))),
    ('ў', above(BREVE, small(b'y'))),
    ('џ', DZHE_SMALL),
    ('א', ALEF),
    ('ב', BET),
    ('ג', GIMEL),
    ('ד', DALET),
    ('ה', HE),
    ('ו', VAV),
    ('ז', ZAYIN),
    ('ח', HET),
    ('ט', TET),
    ('י', YOD),
    ('ך', FINAL_KAF),
    ('כ', KAF),
    ('ל', LAMED),
    ('ם', FINAL_MEM),
    ('מ', MEM),
    ('ן', FINAL_NUN),
    ('נ', NUN),
    ('ס', SAMEKH),
    ('ע', AYIN),
    ('ף', FINAL_PE),
    ('פ', PE),
    ('ץ', FINAL_TSADI),
    ('צ', TSADI),
    ('ק', QOF),
    ('ר', RESH),
    ('ש', SHIN),
    ('ת', TAV),
    ('Ḃ', above(DOT, CAP_B)),
    ('ḃ', B_DOT),
    ('Ḋ', above(DOT, CAP_D)),
    ('ḋ', D_DOT),
    ('Ḟ', above(DOT, CAP_F)),
    ('ḟ', F_DOT),
    ('Ṁ', above(DOT, CAP_M)),
    ('ṁ', above(DOT, small(b'm'))),
    ('Ṗ', above(DOT, CAP_P)),
    ('ṗ', above(DOT, small(b'p'))),
    ('Ṡ', above(DOT, CAP_S)),
    ('ṡ', above(DOT, small(b's'))),
    ('Ṫ', above(DOT, CAP_T)),
    ('ṫ', T_DOT),
    ('Ẁ', above(GRAVE, CAP_W)),
    ('ẁ', above(GRAVE, small(b'w'))),
    ('Ẃ', above(ACUTE, CAP_W)),
    ('ẃ', above(ACUTE, small(b'w'))),
    ('Ẅ', above(DIAERESIS, CAP_W)),
    ('ẅ', above(DIAERESIS, small(b'w'))),
    ('Ỳ', above(GRAVE, CAP_Y)),
    ('ỳ', above(GRAVE, small(b'y'))),
    ('\u{200E}', SPACE),
    ('\u{200F}', SPACE),
    ('―', of(152)),
    ('‗', DOUBLE_LOW_LINE),
    ('‾', OVERLINE),
    ('₯', DRACHMA),
    ('№', NUMERO),
];

/// The characters' glyphs, in the same order.
pub(super) static GLYPHS: [(char, Glyph); 341] = {
    let mut glyphs = [('\0', [0; 8]); 341];
    let mut i = 0;
    while i < DRAWINGS.len() {
        let (character, art) = DRAWINGS[i];
        assert!(
            i == 0 || DRAWINGS[i - 1].0 < character,
            "in order, each once"
        );
        glyphs[i] = (character, draw(art));
        i += 1;
    }
    glyphs
};

/// The glyph `character` has here, if it has one.
pub(super) fn glyph_of(character: char) -> Option<Glyph> {
    let at = GLYPHS.binary_search_by_key(&character, |&(c, _)| c).ok()?;
    GLYPHS.get(at).map(|&(_, glyph)| glyph)
}
