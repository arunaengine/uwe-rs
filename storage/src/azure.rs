//! Signs requests for single Azure blobs with a service SAS made from the account key.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::sync::Arc;

use opendal::raw::{
    OpCompose, OpCopy, OpCreateDir, OpList, OpPresign, OpRead, OpRename, OpRestore, OpStat,
    OpWrite, PresignOperation, PresignedRequest, RpCreateDir, RpPresign, RpRename, RpRestore,
    RpStat, Service, ServiceInfo, Servicer, oio,
};
use opendal::{Capability, Error, ErrorKind, OperationContext, Operator, Result};
use reqsign_azure_storage::{ServiceSasResource, ServiceSharedAccessSignature};
use reqsign_core::time::Timestamp;

/// The version of the Azure Storage service that each signature names.
const VERSION: &str = "2022-11-02";

/// The operator of an Azure container whose presigned requests each grant one blob, for the time
/// asked. Without an account key it signs nothing, as a `sas_token` would give every request all
/// of its rights.
pub(crate) fn sign_blobs(operator: Operator, signer: Signer) -> Operator {
    let service = Blobs {
        inner: operator.service().clone(),
        signer,
    };
    Operator::from_parts(operator.context().clone(), Arc::new(service))
}

/// What a service SAS of one blob needs.
pub(crate) struct Signer {
    /// The account and its key, in Base64.
    pub account: Option<(String, String)>,
    /// The address of the account, like `https://<account>.blob.core.windows.net`.
    pub endpoint: String,
    pub container: String,
    /// The path of the operator in the container, without slashes around it.
    pub root: String,
}

impl Signer {
    /// A request that reads or writes the blob at `path` from `now` until `expire` has passed.
    pub fn sign(&self, path: &str, args: &OpPresign, now: Timestamp) -> Result<PresignedRequest> {
        let Some((account, key)) = &self.account else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Azure Blob Storage signs requests for single files only with the account key",
            ));
        };
        let (method, permissions) = match args.operation() {
            PresignOperation::Read(..) => (http::Method::GET, "r"),
            PresignOperation::Stat(_) => (http::Method::HEAD, "r"),
            PresignOperation::Write(_) => (http::Method::PUT, "cw"),
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "cannot sign this request",
                ));
            }
        };
        let blob = match self.root.is_empty() {
            true => path.trim_start_matches('/').to_owned(),
            false => format!("{}/{}", self.root, path.trim_start_matches('/')),
        };
        let resource = ServiceSasResource::Blob {
            container: self.container.clone(),
            blob: blob.clone(),
        };
        let expiry = now + args.expire();
        let signature = ServiceSharedAccessSignature::new(
            account.clone(),
            key.clone(),
            resource,
            permissions.to_owned(),
            expiry,
        );
        // The key never goes into the error, so the message stays fixed.
        let pairs = (signature.with_version(VERSION).token()).map_err(|_| {
            Error::new(
                ErrorKind::ConfigInvalid,
                "the key of Azure Blob Storage is not Base64",
            )
        })?;
        let invalid = |_| Error::new(ErrorKind::ConfigInvalid, "the endpoint is not a URL");
        let mut url = reqwest::Url::parse(&self.endpoint).map_err(invalid)?;
        (url.path_segments_mut())
            .map_err(|()| Error::new(ErrorKind::ConfigInvalid, "the endpoint is not a URL"))?
            .pop_if_empty()
            .push(&self.container)
            .extend(blob.split('/'));
        url.query_pairs_mut().extend_pairs(&pairs);
        let uri = http::Uri::try_from(url.as_str())
            .map_err(|_| Error::new(ErrorKind::Unexpected, "a signed URL is not valid"))?;
        let mut headers = http::HeaderMap::new();
        if method == http::Method::PUT {
            let block = http::HeaderValue::from_static("BlockBlob");
            headers.insert("x-ms-blob-type", block);
        }
        Ok(PresignedRequest::new(method, uri, headers))
    }
}

/// An Azure container that signs its own requests and passes everything else on.
struct Blobs {
    inner: Servicer,
    signer: Signer,
}

impl fmt::Debug for Blobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blobs")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Service for Blobs {
    type Reader = oio::Reader;
    type Writer = oio::Writer;
    type Lister = oio::Lister;
    type Deleter = oio::Deleter;
    type Copier = oio::Copier;
    type Composer = oio::Composer;

    fn info(&self) -> ServiceInfo {
        self.inner.info()
    }

    fn capability(&self) -> Capability {
        let mut capability = self.inner.capability();
        let signs = self.signer.account.is_some();
        capability.presign = signs;
        capability.presign_read = signs;
        capability.presign_stat = signs;
        capability.presign_write = signs;
        capability
    }

    async fn create_dir(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpCreateDir,
    ) -> Result<RpCreateDir> {
        self.inner.create_dir(ctx, path, args).await
    }

    async fn stat(&self, ctx: &OperationContext, path: &str, args: OpStat) -> Result<RpStat> {
        self.inner.stat(ctx, path, args).await
    }

    fn read(&self, ctx: &OperationContext, path: &str, args: OpRead) -> Result<Self::Reader> {
        self.inner.read(ctx, path, args)
    }

    fn write(&self, ctx: &OperationContext, path: &str, args: OpWrite) -> Result<Self::Writer> {
        self.inner.write(ctx, path, args)
    }

    fn delete(&self, ctx: &OperationContext) -> Result<Self::Deleter> {
        self.inner.delete(ctx)
    }

    fn list(&self, ctx: &OperationContext, path: &str, args: OpList) -> Result<Self::Lister> {
        self.inner.list(ctx, path, args)
    }

    fn copy(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpCopy,
    ) -> Result<Self::Copier> {
        self.inner.copy(ctx, from, to, args)
    }

    fn compose(&self, ctx: &OperationContext, to: &str, args: OpCompose) -> Result<Self::Composer> {
        self.inner.compose(ctx, to, args)
    }

    async fn rename(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpRename,
    ) -> Result<RpRename> {
        self.inner.rename(ctx, from, to, args).await
    }

    async fn restore(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpRestore,
    ) -> Result<RpRestore> {
        self.inner.restore(ctx, path, args).await
    }

    async fn presign(
        &self,
        _: &OperationContext,
        path: &str,
        args: OpPresign,
    ) -> Result<RpPresign> {
        Ok(RpPresign::new(self.signer.sign(
            path,
            &args,
            Timestamp::now(),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The well-known account of the Azurite emulator.
    const KEY: &str =
        "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

    fn signer(key: bool) -> Signer {
        Signer {
            account: key.then(|| ("devstoreaccount1".to_owned(), KEY.to_owned())),
            endpoint: "http://127.0.0.1:10000/devstoreaccount1".into(),
            container: "staging".into(),
            root: "uwe/root".into(),
        }
    }

    #[test]
    fn signs_one_blob() {
        let read = OpPresign::new(OpRead::default(), Duration::from_secs(3600));
        let signed = signer(true)
            .sign("cas/a b", &read, Timestamp::now())
            .unwrap();
        assert_eq!(signed.method(), http::Method::GET);
        let url = reqwest::Url::parse(&signed.uri().to_string()).unwrap();
        assert_eq!(url.path(), "/devstoreaccount1/staging/uwe/root/cas/a%20b");
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query["sr"], "b", "a SAS of one blob");
        assert_eq!(query["sp"], "r");
        assert_eq!(query["sv"], VERSION);
        assert!(!query.contains_key("st"));
        let expiry = Timestamp::now() + Duration::from_secs(3600);
        assert!(query["se"].as_ref() <= expiry.format_rfc3339_zulu().as_str());
        assert_eq!(query["sig"].len(), 44, "{url}");
        let write = OpPresign::new(OpWrite::default(), Duration::from_secs(60));
        let signed = (signer(true).sign("files/1", &write, Timestamp::now())).unwrap();
        assert_eq!(signed.method(), http::Method::PUT);
        assert!(
            signed.uri().to_string().contains("sp=cw"),
            "{}",
            signed.uri()
        );
        assert_eq!(signed.header()["x-ms-blob-type"], "BlockBlob");
        let error = (signer(false).sign("files/1", &write, Timestamp::now())).unwrap_err();
        assert!(error.to_string().contains("account key"), "{error}");
    }

    /// The signature of the string-to-sign of Microsoft's "Create a service SAS", made with
    /// Python's `hmac` for this blob, expiry and version.
    #[test]
    fn matches_known_signature() {
        let signer = Signer {
            root: String::new(),
            ..signer(true)
        };
        let read = OpPresign::new(OpRead::default(), Duration::from_secs(3600));
        let now = Timestamp::from_second(1_800_000_000 - 3600).unwrap();
        let signed = signer.sign("cas/a", &read, now).unwrap();
        let url = reqwest::Url::parse(&signed.uri().to_string()).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query["se"], "2027-01-15T08:00:00Z");
        assert_eq!(query["sig"], "ASQe8I6UNtz29tlx/d2WMie3QX7RUbybwSS/JzQEPts=");
    }

    /// A `sas_token` alone signs nothing, as each request would carry all of its rights.
    #[tokio::test]
    async fn never_shares_token() {
        let objects = |key: Option<&str>| {
            let service = crate::objects::Service::AzureBlobStorage {
                endpoint: "http://127.0.0.1:10000/devstoreaccount1".into(),
                account: Some("devstoreaccount1".into()),
                key: key.map(str::to_owned),
                sas_token: Some("sv=2022-11-02&sp=racwdl&sig=secret".into()),
            };
            crate::objects::Objects::new(service).unwrap()
        };
        let operator = objects(None).at("az://staging/root").unwrap();
        assert!(!operator.info().capability().presign_read);
        let operator = objects(Some(KEY)).at("az://staging/root").unwrap();
        let signed = operator.presign_read("cas/a", Duration::from_secs(60));
        let url = signed.await.unwrap().uri().to_string();
        assert!(url.contains("/staging/root/cas/a?"), "{url}");
        assert!(url.contains("sr=b") && !url.contains("secret"), "{url}");
    }
}
