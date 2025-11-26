dependency "shared_vpc" {
  config_path = "../shared/vpc"
}

locals {
  common_data = yamldecode(file("../data/common.yaml"))
}
