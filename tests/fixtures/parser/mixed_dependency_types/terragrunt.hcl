# Both singular dependency blocks and plural dependencies block
dependency "vpc" {
  config_path = "../vpc"
}

dependency "sg" {
  config_path = "../sg"
  enabled = false
}

dependencies {
  paths = ["../rds", "../elasticache"]
}
