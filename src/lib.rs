use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct TopLevelDepends {
    pub depends: Vec<Depend>,
}

#[derive(Serialize, Deserialize)]
pub struct Depend {
    pub name: String,
    pub version: String,
}
