variable "region" {
  type    = string
  default = "us-east-1"
}

locals {
  bucket_name = "app-${var.region}"
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]
}

resource "aws_instance" "web" {
  ami           = data.aws_ami.ubuntu.id
  instance_type = "t3.micro"
  tags = {
    Name = local.bucket_name
  }
}

resource "aws_eip" "web_ip" {
  instance   = aws_instance.web.id
  depends_on = [aws_instance.web]
}

module "network" {
  source = "./modules/network"
  region = var.region
}

output "instance_id" {
  value = aws_instance.web.id
}
