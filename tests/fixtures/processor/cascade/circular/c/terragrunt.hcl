terraform {
  source = "git::https://github.com/example/module-c.git"
}

# This creates a circular dependency: A -> B -> C -> A
dependency "a" {
  config_path = "../a"
}
