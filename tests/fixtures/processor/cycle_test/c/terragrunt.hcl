# Project C depends on A (closing the cycle)
terraform {
  source = "../modules/c"
}

dependency "a" {
  config_path = "../a"
}
