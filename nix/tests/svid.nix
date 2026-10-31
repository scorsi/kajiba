# The runner channel's test identities (0124), after azukari's test CA: an internal CA's profile
# (ECDSA P-256, one URI SAN and no DNS SAN, digitalSignature, both EKUs, not a CA), built once at
# build time. `<identity>/{svid,key}.pem` per identity, `bundle.pem` the trust bundle; ca.key is
# kept for the subtests that reissue in the VM. `other-ca/runner` holds the runner's ID from an
# unrelated CA; `stranger` is a valid SVID of the trust domain that no runner declares.
{ pkgs }:
let
  id = host: service: "spiffe://example.org/${host}/${service}";
  ca = pkgs.runCommand "kajiba-test-ca" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir -p $out/other-ca
    cat > ca.cnf <<EOF
    [req]
    distinguished_name = dn
    x509_extensions = v3
    [dn]
    [v3]
    basicConstraints = critical,CA:TRUE
    keyUsage = critical,keyCertSign
    subjectKeyIdentifier = hash
    EOF
    mkca() {
      openssl ecparam -name prime256v1 -genkey -noout -out $1.key
      openssl req -x509 -new -config ca.cnf -key $1.key -subj /CN=kajiba-test-$2 -days 3650 -out $1.pem
    }
    issue() { # <ca> <dir> <spiffe id>
      mkdir -p $2
      openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out $2/key.pem
      openssl req -new -key $2/key.pem -subj /O=kajiba-test -out leaf.csr
      printf '%s\n' basicConstraints=critical,CA:FALSE keyUsage=critical,digitalSignature \
        extendedKeyUsage=serverAuth,clientAuth "subjectAltName=URI:$3" > leaf.ext
      openssl x509 -req -in leaf.csr -CA $1.pem -CAkey $1.key -CAcreateserial -days 3650 \
        -extfile leaf.ext -out $2/svid.pem
    }
    mkca $out/ca ca
    mkca other other
    cp $out/ca.pem $out/bundle.pem
    issue $out/ca $out/forge ${id "forge" "kajiba-server"}
    issue $out/ca $out/runner ${id "runner" "kajiba-runner"}
    issue $out/ca $out/runner-2 ${id "runner-2" "kajiba-runner"}
    issue $out/ca $out/stranger ${id "stranger" "kajiba-runner"}
    issue other $out/other-ca/runner ${id "runner" "kajiba-runner"}
    openssl x509 -noout -ext subjectAltName -in $out/runner/svid.pem | grep -q URI:spiffe
  '';
  dir = unit: "/var/lib/hanko/${unit}";
in
{
  inherit ca id;
  serverId = id "forge" "kajiba-server";

  # The module options' file paths for `unit`'s SVID, where hanko writes them (outside the store:
  # the key must not be world-readable).
  files = unit: {
    svidFile = "${dir unit}/svid.pem";
    keyFile = "${dir unit}/key.pem";
    bundleFile = "${dir unit}/bundle.pem";
  };

  # A NixOS module standing in for hanko: before `unit` starts, `identity`'s SVID, key and the
  # bundle are installed in its directory, setgid 2750, files 0640, group `unit`.
  install = unit: identity: {
    systemd.services."test-svid-${unit}" = {
      requiredBy = [ "${unit}.service" ];
      before = [ "${unit}.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        install -d -m 2750 -g ${unit} ${dir unit}
        install -m 0640 -g ${unit} ${ca}/${identity}/svid.pem ${ca}/${identity}/key.pem \
          ${ca}/bundle.pem ${dir unit}/
      '';
    };
  };

  # Python: `reissue(machine, unit, spiffe_id)` issues a new SVID and key from the test CA on
  # `machine` and renames both into `unit`'s directory before any handshake reads them; returns
  # the new certificate's serial.
  reissue = ''


    def reissue(machine, unit, spiffe_id):
        d = f"/var/lib/hanko/{unit}"
        openssl = "${pkgs.openssl}/bin/openssl"
        machine.succeed(
            f"set -e; cd $(mktemp -d);"
            f" {openssl} genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out key.pem;"
            f" {openssl} req -new -key key.pem -subj /O=kajiba-test-reissued -out leaf.csr;"
            " printf '%s\\n' basicConstraints=critical,CA:FALSE keyUsage=critical,digitalSignature"
            f" extendedKeyUsage=serverAuth,clientAuth subjectAltName=URI:{spiffe_id} > leaf.ext;"
            f" {openssl} x509 -req -in leaf.csr -CA ${ca}/ca.pem -CAkey ${ca}/ca.key -CAserial serial"
            " -CAcreateserial -days 3650 -extfile leaf.ext -out svid.pem;"
            f" install -m 0640 -g {unit} svid.pem {d}/svid.pem.new;"
            f" install -m 0640 -g {unit} key.pem {d}/key.pem.new;"
            f" mv {d}/key.pem.new {d}/key.pem; mv {d}/svid.pem.new {d}/svid.pem"
        )
        return machine.succeed(f"{openssl} x509 -noout -serial -in {d}/svid.pem").strip()
  '';
}
