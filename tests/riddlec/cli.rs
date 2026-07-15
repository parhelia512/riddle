use super::*;

#[test]
fn parse_args_accepts_no_std() {
    let args = vec!["riddlec".into(), "--no-std".into(), "main.rid".into()];
    let opts = parse_args(&args).unwrap();

    assert!(!opts.use_std);
    assert_eq!(opts.files, vec!["main.rid"]);
}
