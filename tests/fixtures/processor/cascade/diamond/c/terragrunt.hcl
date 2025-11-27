terraform {
  source = "git::https://github.com/example/module-c.git"
}

dependency "d" {
  config_path = "../d"
}
