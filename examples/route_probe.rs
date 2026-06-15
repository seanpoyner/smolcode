//! Verify the Rust learned classifier loads and predicts.
//! Run: cargo run --example route_probe -- "rebase the branch and fix the conflict"

fn main() {
    let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let task = if task.is_empty() {
        "rebase the feature branch and resolve the merge conflict".to_string()
    } else {
        task
    };
    let learned = smolcode::route_clf::predict_specialty(&task);
    let smart = smolcode::router::classify_specialty_smart(&task);
    let regex = smolcode::router::classify_specialty(&task);
    let tier = smolcode::router::classify_start(&task);
    println!("task:            {task}");
    println!("learned specialty: {learned:?}   (None = abstained -> regex)");
    println!("smart specialty:   {smart}");
    println!("regex specialty:   {regex}");
    println!("start tier:        {tier:?}");
}
