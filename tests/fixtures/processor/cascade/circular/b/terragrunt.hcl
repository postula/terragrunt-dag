terraform {
  source = "git::https://github.com/example/module-b.git"
}

dependency "c" {
  config_path = "../c"
}
