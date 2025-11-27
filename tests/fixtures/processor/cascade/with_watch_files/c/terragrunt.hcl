terraform {
  source = "git::https://github.com/example/module-c.git"
}

# This creates a watch file that should cascade to A when cascade=true
locals {
  data = yamldecode(file("../data.yaml"))
}
