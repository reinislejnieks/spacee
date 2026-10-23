use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    let start = Instant::now();
    let tree = scan::scan(&root, &scan::Progress::default())
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()));
    let elapsed = start.elapsed();

    let top = tree.node(tree.root());
    println!(
        "{}  {} KiB  {} files  {} nodes  {} errors  {:.2?}",
        root.display(),
        top.size / 1024,
        top.file_count,
        tree.node_count(),
        tree.errors.len(),
        elapsed
    );
    for &id in top.children.iter().take(20) {
        let node = tree.node(id);
        println!("{:>14} KiB  {}", node.size / 1024, node.name);
    }
}
