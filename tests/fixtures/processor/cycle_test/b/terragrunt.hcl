# Project B depends on C
terraform {
  source = "../modules/b"
}

dependency "c" {
  config_path = "../c"
}
