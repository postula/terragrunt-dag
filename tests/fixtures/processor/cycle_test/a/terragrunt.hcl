# Project A depends on B (creates cycle A -> B -> C -> A)
terraform {
  source = "../modules/a"
}

dependency "b" {
  config_path = "../b"
}
