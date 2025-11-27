terraform {
  source = "git::https://github.com/example/module-a.git"
}

dependency "b" {
  config_path = "../b"
}
