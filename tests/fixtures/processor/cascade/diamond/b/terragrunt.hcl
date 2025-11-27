terraform {
  source = "git::https://github.com/example/module-b.git"
}

dependency "d" {
  config_path = "../d"
}
